# Hub backfill report

Written by `.github/workflows/hub-packs.yml` (the `backfill report` job) after every
publishing run; do not edit by hand. One row per corpus per run; the rows behind it
are in `rows.jsonl`. Spec: docs/CORPUS_HUB_V2_IMPLEMENTATION.md §12.1.

## Current state

537 corpora attempted: 464 published at their latest attempt, 73 not.

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
| `atoma-run--atoma` | `7c049cbe` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `atomic-agents` | `4d443474` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `attack-flow` | `8b6ca28b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `attack-mappings-explorer` | `a85e55af` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `autogen` | `027ecf0a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `autorag` | `ccb70c12` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `avro` | `28cb08c1` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `awesome-ai-system-prompts` | `74964625` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `axe-core` | `7e287a6a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `axolotl` | `e767fe07` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `dunorth595--ai-agent-chat` | `6386bb68` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--ai-agent-memory` | `97f38fd0` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-id` | `e4a3f385` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-log` | `c3fbc7f4` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-did-mcp` | `8f6304ff` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `fluent-bit` | `3b6ce5cf` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `wysking--postgrad-study-engine` | `94c4b00b` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `wysking--quantitera` | `354b4e44` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `xobotyi--cc-foundry-ai-helpers` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-ai-helpers-prompt-engineering` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-ai-helpers-skill-engineering` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-backend` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-cli` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-git-commit` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-golang` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-grafana` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-infrastructure` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-javascript` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-open-source` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-php` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-python` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-rust` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-the-blueprint` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-the-blueprint-adr` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-the-coder` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-the-workflow-claude-md` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |

## Runs (newest first)

The 30 newest of 44 runs; every row is in `rows.jsonl`.

### Run [41199101229](https://github.com/xerj-org/xerj/actions/runs/41199101229), 2026-10-31: 0 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `atoma-run--atoma` | `7c049cbe` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `dunorth595--ai-agent-chat` | `6386bb68` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--ai-agent-memory` | `97f38fd0` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-id` | `e4a3f385` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-log` | `c3fbc7f4` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-did-mcp` | `8f6304ff` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `wysking--postgrad-study-engine` | `94c4b00b` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `wysking--quantitera` | `354b4e44` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `xobotyi--cc-foundry-ai-helpers` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-ai-helpers-prompt-engineering` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-ai-helpers-skill-engineering` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-backend` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-cli` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-git-commit` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-golang` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-grafana` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-infrastructure` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-javascript` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-open-source` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-php` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-python` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-rust` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-the-blueprint` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-the-blueprint-adr` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-the-coder` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xobotyi--cc-foundry-the-workflow-claude-md` | `b9f5057a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |

### Run [41182803575](https://github.com/xerj-org/xerj/actions/runs/41182803575), 2026-10-31: 1 of 23 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `atoma-run--atoma` | `7c049cbe` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `dunorth595--ai-agent-chat` | `6386bb68` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--ai-agent-memory` | `97f38fd0` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-id` | `e4a3f385` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-log` | `c3fbc7f4` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-did-mcp` | `8f6304ff` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `wysking--postgrad-study-engine` | `94c4b00b` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `wysking--quantitera` | `354b4e44` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mesh-patterns` | `1758bec8` | 245625 | 72 | published `pack-mesh-patterns-1758bec8` |  |

### Run [40954671712](https://github.com/xerj-org/xerj/actions/runs/40954671712), 2026-10-29: 1 of 23 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `atoma-run--atoma` | `7c049cbe` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `dunorth595--ai-agent-chat` | `6386bb68` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--ai-agent-memory` | `97f38fd0` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-id` | `e4a3f385` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-log` | `c3fbc7f4` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-did-mcp` | `8f6304ff` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `wysking--postgrad-study-engine` | `94c4b00b` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `wysking--quantitera` | `354b4e44` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `oxidize-pdf` | `24bc1f84` | 356247697 | 24124 | published `pack-oxidize-pdf-24bc1f84` |  |

### Run [40915906759](https://github.com/xerj-org/xerj/actions/runs/40915906759), 2026-10-29: 0 of 22 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `atoma-run--atoma` | `7c049cbe` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `dunorth595--ai-agent-chat` | `6386bb68` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--ai-agent-memory` | `97f38fd0` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-id` | `e4a3f385` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-log` | `c3fbc7f4` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-did-mcp` | `8f6304ff` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `wysking--postgrad-study-engine` | `94c4b00b` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `wysking--quantitera` | `354b4e44` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |

### Run [40873773018](https://github.com/xerj-org/xerj/actions/runs/40873773018), 2026-10-29: 0 of 22 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `atoma-run--atoma` | `7c049cbe` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `dunorth595--ai-agent-chat` | `6386bb68` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--ai-agent-memory` | `97f38fd0` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-id` | `e4a3f385` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-log` | `c3fbc7f4` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-did-mcp` | `8f6304ff` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `wysking--postgrad-study-engine` | `94c4b00b` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `wysking--quantitera` | `354b4e44` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |

### Run [40833171097](https://github.com/xerj-org/xerj/actions/runs/40833171097), 2026-10-29: 1 of 21 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `atoma-run--atoma` | `7c049cbe` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `dunorth595--ai-agent-chat` | `6386bb68` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--ai-agent-memory` | `97f38fd0` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-id` | `e4a3f385` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-log` | `c3fbc7f4` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-did-mcp` | `8f6304ff` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `texio` | `4ea70985` | 2136084 | 182 | published `pack-texio-4ea70985` |  |

### Run [40803165100](https://github.com/xerj-org/xerj/actions/runs/40803165100), 2026-10-29: 3 of 23 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `atoma-run--atoma` | `7c049cbe` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `dunorth595--ai-agent-chat` | `6386bb68` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--ai-agent-memory` | `97f38fd0` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-id` | `e4a3f385` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-log` | `c3fbc7f4` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-did-mcp` | `8f6304ff` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `faah` | `065a1503` | 1104492 | 187 | published `pack-faah-065a1503` |  |
| `pyverge` | `42cb4096` | 10758078 | 2455 | published `pack-pyverge-42cb4096` |  |
| `vanguard-context-burn` | `e0759ae0` | 142252651 | 3592 | published `pack-vanguard-context-burn-e0759ae0` |  |

### Run [40780715024](https://github.com/xerj-org/xerj/actions/runs/40780715024), 2026-10-28: 0 of 20 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `atoma-run--atoma` | `7c049cbe` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `dunorth595--ai-agent-chat` | `c106b3fe` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--ai-agent-memory` | `97f38fd0` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-id` | `e4a3f385` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-agent-log` | `c3fbc7f4` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `dunorth595--xrpl-did-mcp` | `8f6304ff` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |

### Run [40060994809](https://github.com/xerj-org/xerj/actions/runs/40060994809), 2026-10-24: 3 of 18 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `atoma-run--atoma` | `7c049cbe` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `admission-compare` | `7a439382` | 740654 | 142 | published `pack-admission-compare-7a439382` |  |
| `r2r-enforce` | `10c2f8ff` | 1244768 | 299 | published `pack-r2r-enforce-10c2f8ff` |  |
| `r2r-mcp` | `244ac952` | 990726 | 204 | published `pack-r2r-mcp-244ac952` |  |

### Run [39974224225](https://github.com/xerj-org/xerj/actions/runs/39974224225), 2026-10-23: 0 of 15 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `atoma-run--atoma` | `7c049cbe` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |

### Run [39928880358](https://github.com/xerj-org/xerj/actions/runs/39928880358), 2026-10-23: 1 of 15 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `plan-and-act` | `1c000d6a` | 2553864 | 1875 | published `pack-plan-and-act-1c000d6a` |  |

### Run [39726179289](https://github.com/xerj-org/xerj/actions/runs/39726179289), 2026-10-22: 5 of 7 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `xerj-search` | `3a55bc98` | 960560829 | 579535 | published `pack-xerj-search-3a55bc98` |  |
| `xerj-vector` | `74f3e85b` | 76049973 | 30767 | published `pack-xerj-vector-74f3e85b` |  |
| `xz` | `b355a311` | 29157345 | 2060 | published `pack-xz-b355a311` |  |
| `zalando-restful-api-guidelines` | `88cdfdb1` | 2121473 | 0 | published `pack-zalando-restful-api-guidelines-88cdfdb1` |  |
| `zlib-ng` | `a7f51090` | 12256827 | 1966 | published `pack-zlib-ng-a7f51090` |  |

### Run [39623848815](https://github.com/xerj-org/xerj/actions/runs/39623848815), 2026-10-21: 24 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
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
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `xerj-blogposts` | `a9ad4fc0` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 2) |
| `xerj-columnar` | `31081d9f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `xerj-search` | `3a55bc98` |  |  | cancelled at `build` | autoindex: search failed: search: error sending request for url (http://127.0.0.1:19200/xc-xerj-search-b1792632964-lucene-lucene-2/_search); retrying (attempt 1 of 6, 300 s in this request so far) |
| `ipsum-blocklist` | `444b4802` | 26378221 | 0 | published `pack-ipsum-blocklist-444b4802-r10` |  |
| `retext-passive` | `66ca2c56` | 111704 | 2 | published `pack-retext-passive-66ca2c56-r4` |  |
| `retext-profanities` | `2627fb08` | 130655 | 4 | published `pack-retext-profanities-2627fb08-r4` |  |
| `uslm-us-legislation` | `4ec9db2f` | 56241879 | 0 | published `pack-uslm-us-legislation-4ec9db2f` |  |
| `valkey-docs` | `4852e820` | 11592071 | 51 | published `pack-valkey-docs-4852e820` |  |
| `valkey-src` | `f4cbd1c5` | 75901715 | 27272 | published `pack-valkey-src-f4cbd1c5` |  |
| `vectra-core` | `af64231d` | 516921 | 207 | published `pack-vectra-core-af64231d` |  |
| `vite` | `dce9fe69` | 7541133 | 3992 | published `pack-vite-dce9fe69` |  |
| `vue` | `ff350142` | 18014288 | 5378 | published `pack-vue-ff350142` |  |
| `vuln-fix-commits` | `3f7ce8ce` | 3948835 | 1074 | published `pack-vuln-fix-commits-3f7ce8ce` |  |
| `w3c-css` | `d06dc03e` | 100341798 | 1677 | published `pack-w3c-css-d06dc03e` |  |
| `weaviate` | `a920eab8` | 155918974 | 73057 | published `pack-weaviate-a920eab8` |  |
| `web-features` | `de89ce66` | 11634883 | 215 | published `pack-web-features-de89ce66` |  |
| `webgl` | `714857a2` | 54370970 | 9236 | published `pack-webgl-714857a2` |  |
| `whatwg-dom` | `3071e5f2` | 197469 | 0 | published `pack-whatwg-dom-3071e5f2` |  |
| `whatwg-fetch` | `e9460d1b` | 153072 | 0 | published `pack-whatwg-fetch-e9460d1b` |  |
| `whatwg-html` | `86190a4d` | 1009130 | 47 | published `pack-whatwg-html-86190a4d` |  |
| `whatwg-specs` | `4d844936` | 31336581 | 25 | published `pack-whatwg-specs-4d844936` |  |
| `whatwg-streams` | `b9ba9f49` | 8795033 | 559 | published `pack-whatwg-streams-b9ba9f49` |  |
| `whatwg-url` | `fde3f74f` | 148326 | 0 | published `pack-whatwg-url-fde3f74f` |  |
| `whatwg-webidl` | `0620cf17` | 226514 | 3 | published `pack-whatwg-webidl-0620cf17` |  |
| `woke-inclusive-lint` | `5d52c154` | 1200106 | 333 | published `pack-woke-inclusive-lint-5d52c154` |  |
| `write-good` | `6940b034` | 797105 | 11 | published `pack-write-good-6940b034` |  |
| `xerj-storage` | `e449d171` | 26441078 | 4408 | published `pack-xerj-storage-e449d171` |  |

### Run [39605736840](https://github.com/xerj-org/xerj/actions/runs/39605736840), 2026-10-21: 24 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `fluent-bit` | `3b6ce5cf` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | failed at `build` | xerj hub build-pack: bulk /xc-freebsd-handbook-b1792591697-defs/_bulk -> HTTP 429: {"took":4,"errors":true,"items":[{"index":{"_index":"xc-freebsd-handbook-b1792591697-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed" |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ipsum-blocklist` | `444b4802` | 26518051 | 0 | failed at `e2e` | no question from ask_for (reputation |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `retext-passive` | `66ca2c56` | 111438 | 2 | failed at `e2e` | no question from ask_for (constructions |
| `retext-profanities` | `2627fb08` | 130993 | 4 | failed at `e2e` | no question from ask_for (rephrasings |
| `stripe-openapi` | `e73367db` | 342943723 | 0 | published `pack-stripe-openapi-e73367db` |  |
| `style-dictionary` | `81ecc052` | 11495097 | 805 | published `pack-style-dictionary-81ecc052` |  |
| `svelte` | `6480a973` | 22458345 | 6136 | published `pack-svelte-6480a973` |  |
| `swagger-ui` | `e89a91f1` | 13720804 | 12731 | published `pack-swagger-ui-e89a91f1` |  |
| `swift-book` | `1c059843` | 7701781 | 4 | published `pack-swift-book-1c059843` |  |
| `swift-evolution` | `239be1db` | 7336697 | 0 | published `pack-swift-evolution-239be1db` |  |
| `symptom-ontology` | `7a295d98` | 6125576 | 19 | published `pack-symptom-ontology-7a295d98` |  |
| `synthea` | `d9d07a6e` | 117324295 | 4372 | published `pack-synthea-d9d07a6e` |  |
| `textlint` | `2bc95477` | 12353868 | 1075 | published `pack-textlint-2bc95477` |  |
| `textlint-ja-technical-writing` | `5e73caaf` | 2125523 | 0 | published `pack-textlint-ja-technical-writing-5e73caaf` |  |
| `textlint-rule-preset-ai-writing` | `45bb6485` | 2822860 | 17 | published `pack-textlint-rule-preset-ai-writing-45bb6485` |  |
| `textlint-rule-preset-jtf-style` | `2186273a` | 514170 | 55 | published `pack-textlint-rule-preset-jtf-style-2186273a` |  |
| `textlint-rule-stop-words` | `674119e6` | 1806028 | 12 | published `pack-textlint-rule-stop-words-674119e6` |  |
| `textlint-rule-terminology` | `b866c04b` | 942665 | 13 | published `pack-textlint-rule-terminology-b866c04b` |  |
| `textstat` | `30303f95` | 1522209 | 223 | published `pack-textstat-30303f95` |  |
| `thrift` | `a81db74f` | 65662275 | 23999 | published `pack-thrift-a81db74f` |  |
| `tikv` | `5f7037a4` | 91266066 | 31976 | published `pack-tikv-5f7037a4` |  |
| `tldr-pages` | `87441be0` | 11588505 | 0 | published `pack-tldr-pages-87441be0` |  |
| `trivy-checks` | `ac856458` | 11316447 | 195 | published `pack-trivy-checks-ac856458` |  |
| `twelve-factor` | `1385d2c8` | 12840741 | 704 | published `pack-twelve-factor-1385d2c8` |  |
| `uk-building-regs` | `d4669402` | 2287809 | 25 | published `pack-uk-building-regs-d4669402` |  |
| `uk-legislation` | `0575db1c` | 248411397 | 0 | published `pack-uk-legislation-0575db1c` |  |
| `unicode-cldr` | `cafab85f` | 9488395 | 0 | published `pack-unicode-cldr-cafab85f` |  |
| `unitedstates-citation` | `953c03d3` | 7332839 | 1522 | published `pack-unitedstates-citation-953c03d3` |  |

### Run [39582439994](https://github.com/xerj-org/xerj/actions/runs/39582439994), 2026-10-21: 23 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
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
| `ipsum-blocklist` | `444b4802` | 26332846 | 0 | failed at `e2e` | no question from ask_for (reputation |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | failed at `build` | autoindex: _bulk failed: bulk send (request timeout 300s): error sending request for url (http://127.0.0.1:19200/_bulk): operation timed out; retrying (attempt 1 of 6, 373 s in this request so far) |
| `retext-passive` | `66ca2c56` | 111959 | 2 | failed at `e2e` | no question from ask_for (constructions |
| `retext-profanities` | `2627fb08` | 130498 | 4 | failed at `e2e` | no question from ask_for (rephrasings |
| `stripe-openapi` | `e73367db` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `milvus` | `b48661d3` | 283395080 | 122468 | published `pack-milvus-b48661d3` |  |
| `rustls` | `7dcbe4c1` | 10229482 | 5070 | published `pack-rustls-7dcbe4c1` |  |
| `s2n-tls` | `b1700000` | 58375808 | 26189 | published `pack-s2n-tls-b1700000` |  |
| `sass-spec` | `57c81dd7` | 7385059 | 162 | published `pack-sass-spec-57c81dd7` |  |
| `scap-security-guide` | `f0f3396f` | 108454940 | 3975 | published `pack-scap-security-guide-f0f3396f` |  |
| `school-of-sre` | `22979a6e` | 3602564 | 0 | published `pack-school-of-sre-22979a6e` |  |
| `scispacy` | `eacccd4e` | 1646644 | 239 | published `pack-scispacy-eacccd4e` |  |
| `season-of-docs` | `ec6161e9` | 3736800 | 0 | published `pack-season-of-docs-ec6161e9` |  |
| `serverless-workflow-spec` | `13b6a625` | 1695602 | 6 | published `pack-serverless-workflow-spec-13b6a625` |  |
| `simonw-llm` | `05d7ae7d` | 6939656 | 2032 | published `pack-simonw-llm-05d7ae7d` |  |
| `siren-hypermedia` | `c29a8784` | 143165 | 0 | published `pack-siren-hypermedia-c29a8784` |  |
| `smallstep-certificates` | `69d3c36f` | 5941816 | 5194 | published `pack-smallstep-certificates-69d3c36f` |  |
| `smithy` | `2a61b43f` | 66692219 | 22876 | published `pack-smithy-2a61b43f` |  |
| `solid` | `25138e1c` | 4167902 | 1151 | published `pack-solid-25138e1c` |  |
| `sourcegraph-handbook` | `5752928b` | 7958415 | 109 | published `pack-sourcegraph-handbook-5752928b` |  |
| `spdx-spec` | `395d06e1` | 9482359 | 10 | published `pack-spdx-spec-395d06e1` |  |
| `spectral` | `62527eec` | 12918859 | 5007 | published `pack-spectral-62527eec` |  |
| `spectrum-web-components` | `be922808` | 92356834 | 11482 | published `pack-spectrum-web-components-be922808` |  |
| `splunk-security-content` | `c653da7c` | 41621103 | 1222 | published `pack-splunk-security-content-c653da7c` |  |
| `sqlite-src` | `9696acb0` | 114724778 | 23629 | published `pack-sqlite-src-9696acb0` |  |
| `standard-readme` | `5d18ad4d` | 213365 | 0 | published `pack-standard-readme-5d18ad4d` |  |
| `standard-webhooks` | `7537d2a2` | 583264 | 322 | published `pack-standard-webhooks-7537d2a2` |  |
| `stratus-red-team` | `3ff51c44` | 11583379 | 1232 | published `pack-stratus-red-team-3ff51c44` |  |

### Run [39557589186](https://github.com/xerj-org/xerj/actions/runs/39557589186), 2026-10-21: 23 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
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
| `ipsum-blocklist` | `444b4802` | 26384581 | 0 | failed at `e2e` | no question from ask_for (reputation |
| `milvus` | `b48661d3` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `retext-passive` | `66ca2c56` | 111383 | 2 | failed at `e2e` | no question from ask_for (constructions |
| `retext-profanities` | `2627fb08` | 131129 | 4 | failed at `e2e` | no question from ask_for (rephrasings |
| `rabbitmq` | `f23f19da` | 91591320 | 51154 | published `pack-rabbitmq-f23f19da` |  |
| `raft-rs` | `ad13f3d9` | 2712152 | 937 | published `pack-raft-rs-ad13f3d9` |  |
| `react-spectrum` | `9b7a06ee` | 64114145 | 17448 | published `pack-react-spectrum-9b7a06ee` |  |
| `redhat-vale-style` | `7cfb4abc` | 601163 | 98 | published `pack-redhat-vale-style-7cfb4abc` |  |
| `redoc` | `2e30cabf` | 10301579 | 840 | published `pack-redoc-2e30cabf` |  |
| `redocly-cli` | `7df8cf3c` | 55516187 | 5552 | published `pack-redocly-cli-7df8cf3c` |  |
| `reporters-db` | `7a5b9b06` | 4795925 | 52 | published `pack-reporters-db-7a5b9b06` |  |
| `retext-equality` | `192deddf` | 735016 | 10 | published `pack-retext-equality-192deddf` |  |
| `retext-indefinite-article` | `960875ee` | 138940 | 9 | published `pack-retext-indefinite-article-960875ee` |  |
| `retext-intensify` | `9450bee5` | 101862 | 1 | published `pack-retext-intensify-9450bee5` |  |
| `retext-quotes` | `e43bb5f3` | 147342 | 5 | published `pack-retext-quotes-e43bb5f3` |  |
| `retext-readability` | `ece96241` | 135129 | 4 | published `pack-retext-readability-ece96241` |  |
| `retext-redundant-acronyms` | `e670637a` | 129750 | 2 | published `pack-retext-redundant-acronyms-e670637a` |  |
| `retext-repeated-words` | `d6d53fb6` | 105449 | 2 | published `pack-retext-repeated-words-d6d53fb6` |  |
| `retext-simplify` | `c7686ac9` | 158181 | 2 | published `pack-retext-simplify-c7686ac9` |  |
| `rocksdb` | `4cd0450f` | 117566057 | 47679 | published `pack-rocksdb-4cd0450f` |  |
| `rolldown` | `2d44fc3f` | 61301137 | 18497 | published `pack-rolldown-2d44fc3f` |  |
| `rousseau` | `e7fb314d` | 312275 | 10 | published `pack-rousseau-e7fb314d` |  |
| `rspack` | `7d83e469` | 78782047 | 35492 | published `pack-rspack-7d83e469` |  |
| `rust-api-guidelines` | `97a0969c` | 618133 | 0 | published `pack-rust-api-guidelines-97a0969c` |  |
| `rust-cssparser` | `9f3ceddf` | 5985677 | 500 | published `pack-rust-cssparser-9f3ceddf` |  |
| `rust-url` | `00a6ce58` | 5606784 | 713 | published `pack-rust-url-00a6ce58` |  |
| `rustcrypto-hashes` | `138dc5a0` | 3173331 | 1012 | published `pack-rustcrypto-hashes-138dc5a0` |  |

### Run [39535889582](https://github.com/xerj-org/xerj/actions/runs/39535889582), 2026-10-21: 25 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
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
| `ipsum-blocklist` | `444b4802` | 26408102 | 0 | failed at `e2e` | no question from ask_for (reputation |
| `milvus` | `b48661d3` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cassandra` | `ec2c276b` | 267729959 | 111305 | published `pack-cassandra-ec2c276b` |  |
| `pagerduty-incident-response` | `464fc9d3` | 1503555 | 0 | published `pack-pagerduty-incident-response-464fc9d3` |  |
| `parquet-format` | `bf099392` | 1100478 | 0 | published `pack-parquet-format-bf099392` |  |
| `parse5` | `103d9aa6` | 3186850 | 708 | published `pack-parse5-103d9aa6` |  |
| `payloadsallthethings` | `3ac27901` | 10502050 | 92 | published `pack-payloadsallthethings-3ac27901` |  |
| `pebble` | `e0818dd0` | 29843166 | 13823 | published `pack-pebble-e0818dd0` |  |
| `plain-language-guidelines` | `fd769474` | 7263361 | 2 | published `pack-plain-language-guidelines-fd769474` |  |
| `polars` | `099c2863` | 95464284 | 46897 | published `pack-polars-099c2863` |  |
| `postcss` | `f1aca7ec` | 2376212 | 537 | published `pack-postcss-f1aca7ec` |  |
| `postgres-src` | `50d6e533` | 349981745 | 69477 | published `pack-postgres-src-50d6e533` |  |
| `postman-collection` | `8801e7f4` | 4336144 | 241 | published `pack-postman-collection-8801e7f4` |  |
| `preact` | `8ff8785b` | 5605372 | 3487 | published `pack-preact-8ff8785b` |  |
| `prism` | `2a1b2052` | 7051370 | 462 | published `pack-prism-2a1b2052` |  |
| `prometheus-src` | `7c741e28` | 52747135 | 15251 | published `pack-prometheus-src-7c741e28` |  |
| `prompt-engineering-guide` | `57673726` | 4247694 | 52 | published `pack-prompt-engineering-guide-57673726` |  |
| `promptify` | `bc5ed081` | 869858 | 266 | published `pack-promptify-bc5ed081` |  |
| `proselint` | `dbed789c` | 8110886 | 1697 | published `pack-proselint-dbed789c` |  |
| `protobuf` | `fb0a6f67` | 136322177 | 90906 | published `pack-protobuf-fb0a6f67` |  |
| `protovalidate` | `c550962b` | 10521984 | 8059 | published `pack-protovalidate-c550962b` |  |
| `pubhealth-fact-checking` | `02136d3e` | 1198387 | 17 | published `pack-pubhealth-fact-checking-02136d3e` |  |
| `pubmedqa` | `1cbae8e9` | 10747743 | 20 | published `pack-pubmedqa-1cbae8e9` |  |
| `pysec` | `3373aa62` | 63738469 | 0 | published `pack-pysec-3373aa62` |  |
| `quiche` | `b60aff93` | 7193128 | 6307 | published `pack-quiche-b60aff93` |  |
| `quinn` | `7f1c6b55` | 2794340 | 2868 | published `pack-quinn-7f1c6b55` |  |
| `qwik` | `09c73b97` | 27846285 | 11299 | published `pack-qwik-09c73b97` |  |

### Run [39513172739](https://github.com/xerj-org/xerj/actions/runs/39513172739), 2026-10-21: 24 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
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
| `ipsum-blocklist` | `444b4802` | 26405336 | 0 | failed at `e2e` | no question from ask_for (reputation |
| `milvus` | `b48661d3` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nuclei-templates` | `24a43e14` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `nats-server` | `a44c356c` | 43216305 | 13455 | published `pack-nats-server-a44c356c` |  |
| `navrules` | `4d535668` | 2519997 | 0 | published `pack-navrules-4d535668` |  |
| `nghttp2` | `19d06e62` | 15574154 | 4924 | published `pack-nghttp2-19d06e62` |  |
| `nginx` | `2b5c2b60` | 22525044 | 10876 | published `pack-nginx-2b5c2b60` |  |
| `nigeria-standard-treatment-guidelines` | `d2be90e9` | 6000269 | 0 | published `pack-nigeria-standard-treatment-guidelines-d2be90e9` |  |
| `oasdiff` | `b687d634` | 18149071 | 4783 | published `pack-oasdiff-b687d634` |  |
| `openapi-moonwalk` | `62520aa3` | 670517 | 0 | published `pack-openapi-moonwalk-62520aa3` |  |
| `openapi-overlay-spec` | `d4cc4c80` | 1590887 | 2 | published `pack-openapi-overlay-spec-d4cc4c80` |  |
| `openapi-spec` | `447c479c` | 9414780 | 2 | published `pack-openapi-spec-447c479c` |  |
| `openbao` | `a4f7e569` | 37949025 | 22933 | published `pack-openbao-a4f7e569` |  |
| `openbsd-faq` | `cab1d910` | 109164459 | 491 | published `pack-openbsd-faq-cab1d910` |  |
| `openfda` | `fdbe5432` | 67731092 | 1548 | published `pack-openfda-fdbe5432` |  |
| `openlegaldata-oldp` | `b1c2de58` | 42894545 | 4792 | published `pack-openlegaldata-oldp-b1c2de58` |  |
| `opennyai-legal-nlp` | `2abee8fc` | 9075069 | 703 | published `pack-opennyai-legal-nlp-2abee8fc` |  |
| `opensanctions` | `d1619f4c` | 71791493 | 6489 | published `pack-opensanctions-d1619f4c` |  |
| `openssl` | `7ef69a72` | 216383481 | 52319 | published `pack-openssl-7ef69a72` |  |
| `osquery-config-examples` | `89c2ae5c` | 627706 | 0 | published `pack-osquery-config-examples-89c2ae5c` |  |
| `oss-attribution-builder` | `c8bc98e1` | 4141565 | 404 | published `pack-oss-attribution-builder-c8bc98e1` |  |
| `oss-license-texts` | `f717b235` | 3824218 | 49 | published `pack-oss-license-texts-f717b235` |  |
| `oss-review-toolkit` | `b0e622b0` | 146054098 | 7971 | published `pack-oss-review-toolkit-b0e622b0` |  |
| `otel-proto` | `b3f75588` | 16407181 | 6 | published `pack-otel-proto-b3f75588` |  |
| `owasp-cheatsheets` | `ca1bded7` | 4684596 | 152 | published `pack-owasp-cheatsheets-ca1bded7` |  |
| `owasp-wstg` | `8df290a3` | 8441398 | 244 | published `pack-owasp-wstg-8df290a3` |  |
| `oxc` | `da692b86` | 184992378 | 75128 | published `pack-oxc-da692b86` |  |

### Run [39482496974](https://github.com/xerj-org/xerj/actions/runs/39482496974), 2026-10-20: 25 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
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
| `ipsum-blocklist` | `444b4802` | 26404966 | 0 | failed at `e2e` | no question from ask_for (reputation |
| `milvus` | `b48661d3` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `mtl-bioinformatics-ner` | `df0df02f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `medqa-usmle` | `27b02f66` | 149178 | 121 | published `pack-medqa-usmle-27b02f66` |  |
| `medquad` | `577bd37b` | 88650635 | 0 | published `pack-medquad-577bd37b` |  |
| `medspacy` | `b7bb7d82` | 6917570 | 750 | published `pack-medspacy-b7bb7d82` |  |
| `microsoft-seal` | `31fc3194` | 11063662 | 3558 | published `pack-microsoft-seal-31fc3194` |  |
| `microsoft-style-vale` | `ea5fa73c` | 584457 | 10 | published `pack-microsoft-style-vale-ea5fa73c` |  |
| `miekg-dns` | `2e4905fb` | 3378795 | 2231 | published `pack-miekg-dns-2e4905fb` |  |
| `minify` | `eb78925d` | 7974057 | 2618 | published `pack-minify-eb78925d` |  |
| `minisign` | `b6de4fe3` | 449876 | 161 | published `pack-minisign-b6de4fe3` |  |
| `misp-galaxy` | `b9675983` | 110627262 | 2150 | published `pack-misp-galaxy-b9675983` |  |
| `mitre-attck` | `ec93deaf` | 9047585 | 0 | published `pack-mitre-attck-ec93deaf` |  |
| `mitre-capec` | `c59d35cd` | 4204893 | 9 | published `pack-mitre-capec-c59d35cd` |  |
| `mitre-cwe` | `364b6e86` | 9267032 | 10 | published `pack-mitre-cwe-364b6e86` |  |
| `mkcert` | `1c1dc4ed` | 312459 | 87 | published `pack-mkcert-1c1dc4ed` |  |
| `mls-rs` | `d4c5b19f` | 29012442 | 5215 | published `pack-mls-rs-d4c5b19f` |  |
| `monocypher` | `e65d2772` | 5208607 | 1469 | published `pack-monocypher-e65d2772` |  |
| `mozilla-diversity` | `5e612f24` | 714054 | 0 | published `pack-mozilla-diversity-5e612f24` |  |
| `mozilla-l10n-docs` | `b190a660` | 1171515 | 11 | published `pack-mozilla-l10n-docs-b190a660` |  |
| `mozilla-l10n-styleguides` | `50ad513b` | 5232791 | 17 | published `pack-mozilla-l10n-styleguides-50ad513b` |  |
| `mozilla-ssl-configs` | `34d8441d` | 181914 | 0 | published `pack-mozilla-ssl-configs-34d8441d` |  |
| `moztw-styleguides` | `37d9832a` | 5001165 | 17 | published `pack-moztw-styleguides-37d9832a` |  |
| `msft-api-guidelines` | `a7022a29` | 2586992 | 0 | published `pack-msft-api-guidelines-a7022a29` |  |
| `msgpack-c` | `1cdc1b66` | 129558 | 0 | published `pack-msgpack-c-1cdc1b66` |  |
| `mts-dialog-clinical-notes` | `3ff08019` | 33070524 | 0 | published `pack-mts-dialog-clinical-notes-3ff08019` |  |
| `mutcd` | `b70b9188` | 23223080 | 0 | published `pack-mutcd-b70b9188` |  |
| `nats-protocol` | `1cb267ca` | 2972212 | 11 | published `pack-nats-protocol-1cb267ca` |  |

### Run [39454084138](https://github.com/xerj-org/xerj/actions/runs/39454084138), 2026-10-20: 27 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
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
| `ipsum-blocklist` | `444b4802` | 26362352 | 0 | failed at `e2e` | no question from ask_for (reputation |
| `kyverno-policies` | `ec478cb5` | 1761922 | 0 | published `pack-kyverno-policies-ec478cb5-r2` |  |
| `ldnoobw-bad-words` | `5faf2ba4` | 225193 | 0 | published `pack-ldnoobw-bad-words-5faf2ba4` |  |
| `lego` | `748e608a` | 24518009 | 11869 | published `pack-lego-748e608a` |  |
| `leveldb` | `7ee830d0` | 3708922 | 1870 | published `pack-leveldb-7ee830d0` |  |
| `lexbuild` | `8cc0b93b` | 11085419 | 1009 | published `pack-lexbuild-8cc0b93b` |  |
| `libcbor` | `ab8c72b1` | 3886704 | 1716 | published `pack-libcbor-ab8c72b1` |  |
| `libhydrogen` | `264bf429` | 672746 | 351 | published `pack-libhydrogen-264bf429` |  |
| `libopenapi` | `c4c4be74` | 62741994 | 11360 | published `pack-libopenapi-c4c4be74` |  |
| `libsodium` | `61478971` | 11086618 | 4320 | published `pack-libsodium-61478971` |  |
| `libuv` | `49b1c064` | 7754716 | 8259 | published `pack-libuv-49b1c064` |  |
| `licensee` | `9261af40` | 3461400 | 588 | published `pack-licensee-9261af40` |  |
| `lightningcss` | `ab14f90d` | 11069855 | 4088 | published `pack-lightningcss-ab14f90d` |  |
| `lit` | `01dbc667` | 24454051 | 7300 | published `pack-lit-01dbc667` |  |
| `llhttp` | `69936dbc` | 2027695 | 322 | published `pack-llhttp-69936dbc` |  |
| `llm-course` | `7abd96e8` | 475872 | 0 | published `pack-llm-course-7abd96e8` |  |
| `lmdb` | `700e10f9` | 2429570 | 873 | published `pack-lmdb-700e10f9` |  |
| `lol-html` | `608cc4a6` | 4963167 | 1873 | published `pack-lol-html-608cc4a6` |  |
| `loldrivers` | `6f6f5b49` | 138928678 | 283 | published `pack-loldrivers-6f6f5b49` |  |
| `markdownlint` | `3f1f4793` | 12806147 | 262 | published `pack-markdownlint-3f1f4793` |  |
| `mashqa` | `86e0a7d7` | 1926365 | 675 | published `pack-mashqa-86e0a7d7` |  |
| `material-components-web` | `6ceadb81` | 26068507 | 2390 | published `pack-material-components-web-6ceadb81` |  |
| `material-web` | `e3471c04` | 19361409 | 2514 | published `pack-material-web-e3471c04` |  |
| `mbedtls` | `de23569a` | 22607262 | 4192 | published `pack-mbedtls-de23569a` |  |
| `mdn-browser-compat-data` | `85c50b0b` | 72394960 | 499 | published `pack-mdn-browser-compat-data-85c50b0b` |  |
| `mdn-data` | `2c16ea05` | 2862912 | 3 | published `pack-mdn-data-2c16ea05` |  |
| `medical-abbreviations` | `3970d38b` | 537513 | 0 | published `pack-medical-abbreviations-3970d38b` |  |
| `medmcqa` | `c59ef14c` | 301169 | 27 | published `pack-medmcqa-c59ef14c` |  |

### Run [39419437459](https://github.com/xerj-org/xerj/actions/runs/39419437459), 2026-10-20: 26 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `exploit-pocs-2023` | `af9e194d` |  |  | cancelled at `build` | [FAIL] django-security-advisory-3.2.16: git fetch 4ac74feb7ad380decf41d37cf796b410262b64fd failed; continuing |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `fluent-bit` | `3b6ce5cf` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ipsum-blocklist` | `444b4802` | 26449698 | 0 | failed at `e2e` | no question from ask_for (reputation |
| `kyverno-policies` | `ec478cb5` | 1761907 | 0 | failed at `e2e` | release not found |
| `joblint` | `3b3fbb4e` | 297118 | 301 | published `pack-joblint-3b3fbb4e` |  |
| `jose-js` | `2a3a5259` | 5586921 | 704 | published `pack-jose-js-2a3a5259` |  |
| `jsdom` | `a78f9c22` | 34917196 | 2768 | published `pack-jsdom-a78f9c22` |  |
| `json-api-spec` | `bde79ceb` | 5621859 | 253 | published `pack-json-api-spec-bde79ceb` |  |
| `json-schema-spec` | `4f56a990` | 3032450 | 8 | published `pack-json-schema-spec-4f56a990` |  |
| `json-schema-test-suite` | `8568bcc4` | 4244252 | 0 | published `pack-json-schema-test-suite-8568bcc4` |  |
| `jsonformer` | `bfad0318` | 924048 | 23 | published `pack-jsonformer-bfad0318` |  |
| `jsonwebtoken-rust` | `11f83ab5` | 1011440 | 420 | published `pack-jsonwebtoken-rust-11f83ab5` |  |
| `jwx` | `c4774435` | 12454487 | 3851 | published `pack-jwx-c4774435` |  |
| `kafka-protocol` | `94ac9e02` | 6206844 | 0 | published `pack-kafka-protocol-94ac9e02` |  |
| `kafka-src` | `d12941eb` | 190172568 | 107799 | published `pack-kafka-src-d12941eb` |  |
| `karma` | `38fd3a9f` | 16101745 | 1733 | published `pack-karma-38fd3a9f` |  |
| `karpenter` | `a4432485` | 23951803 | 4110 | published `pack-karpenter-a4432485` |  |
| `keda` | `59815e11` | 26368495 | 11825 | published `pack-keda-59815e11` |  |
| `keep-a-changelog` | `08d0df5a` | 3503911 | 288 | published `pack-keep-a-changelog-08d0df5a` |  |
| `keywhiz` | `52e77fd9` | 7751429 | 4349 | published `pack-keywhiz-52e77fd9` |  |
| `kics` | `bbb3c148` | 90263370 | 3867 | published `pack-kics-bbb3c148` |  |
| `knox` | `61dd5823` | 1892732 | 848 | published `pack-knox-61dd5823` |  |
| `kotaemon` | `9ad3e4e4` | 5614362 | 2158 | published `pack-kotaemon-9ad3e4e4` |  |
| `kratix` | `2ed71d24` | 14161248 | 2268 | published `pack-kratix-2ed71d24` |  |
| `kryptology` | `eef70332` | 4517679 | 4240 | published `pack-kryptology-eef70332` |  |
| `kube-bench` | `e278a591` | 8509449 | 306 | published `pack-kube-bench-e278a591` |  |
| `kube-hunter` | `bc47f08e` | 1565146 | 741 | published `pack-kube-hunter-bc47f08e` |  |
| `kubernetes-community` | `585a4ac6` | 63482596 | 170 | published `pack-kubernetes-community-585a4ac6` |  |
| `kubescape-regolibrary` | `5d5fd8fc` | 13187671 | 382 | published `pack-kubescape-regolibrary-5d5fd8fc` |  |
| `lambdaworks` | `3a87850d` | 9444212 | 9088 | published `pack-lambdaworks-3a87850d` |  |

### Run [39335555740](https://github.com/xerj-org/xerj/actions/runs/39335555740), 2026-10-19: 27 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
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
| `ipsum-blocklist` | `444b4802` | 26583768 | 0 | failed at `e2e` | no question from ask_for (reputation |
| `biome` | `39d7595b` | 232702957 | 107317 | published `pack-biome-39d7595b` |  |
| `googleapis` | `dfaf98d4` | 320918461 | 37 | published `pack-googleapis-dfaf98d4` |  |
| `h2` | `e64c1662` | 66863481 | 1948 | published `pack-h2-e64c1662` |  |
| `h2o` | `d21e7cb5` | 70215323 | 24686 | published `pack-h2o-d21e7cb5` |  |
| `hacl-star` | `504c2987` | 62683093 | 16889 | published `pack-hacl-star-504c2987` |  |
| `harbor` | `23bfa9a1` | 54233081 | 18043 | published `pack-harbor-23bfa9a1` |  |
| `harper` | `6a0f42d1` | 25758119 | 13709 | published `pack-harper-6a0f42d1` |  |
| `haystack` | `441023dc` | 38627674 | 9557 | published `pack-haystack-441023dc` |  |
| `head-qa` | `da54ff23` | 348559 | 104 | published `pack-head-qa-da54ff23` |  |
| `helm` | `dc56a44f` | 13864788 | 4471 | published `pack-helm-dc56a44f` |  |
| `hivemq-mqtt-client` | `99ab2013` | 11512829 | 7434 | published `pack-hivemq-mqtt-client-99ab2013` |  |
| `how-they-sre` | `284703b1` | 1030499 | 1 | published `pack-how-they-sre-284703b1` |  |
| `httparse` | `a0fa552e` | 733791 | 301 | published `pack-httparse-a0fa552e` |  |
| `httpx` | `b5addb64` | 2986507 | 1335 | published `pack-httpx-b5addb64` |  |
| `huggingface-hub-docs` | `fa1174ae` | 16452015 | 42 | published `pack-huggingface-hub-docs-fa1174ae` |  |
| `hyper` | `2b454af2` | 5095340 | 2176 | published `pack-hyper-2b454af2` |  |
| `ibm-openapi-validator` | `cf6ec487` | 8770807 | 369 | published `pack-ibm-openapi-validator-cf6ec487` |  |
| `icd10-who` | `bff0a1dc` | 2362152 | 121 | published `pack-icd10-who-bff0a1dc` |  |
| `icd10cm-codes` | `98ad49ef` | 21156520 | 40 | published `pack-icd10cm-codes-98ad49ef` |  |
| `icd9-icd10-hierarchy` | `1b03a8c0` | 34469561 | 0 | published `pack-icd9-icd10-hierarchy-1b03a8c0` |  |
| `iceberg` | `8102f4cb` | 125981560 | 93392 | published `pack-iceberg-8102f4cb` |  |
| `in-solidarity-bot` | `9f069c62` | 2790122 | 43 | published `pack-in-solidarity-bot-9f069c62` |  |
| `influxdb` | `06200ef9` | 92123891 | 21488 | published `pack-influxdb-06200ef9` |  |
| `infracost` | `ccc476fb` | 33012153 | 6110 | published `pack-infracost-ccc476fb` |  |
| `inspect-ai` | `81c1668d` | 121530391 | 37678 | published `pack-inspect-ai-81c1668d` |  |
| `instructor` | `035c77b7` | 27758641 | 5904 | published `pack-instructor-035c77b7` |  |
| `jaeger-docs` | `231b67c0` | 23494060 | 57 | published `pack-jaeger-docs-231b67c0` |  |

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
