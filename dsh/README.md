# xerj-dsh

DeepSeek Harness ([dsh](https://github.com/deepseek-ai/deepseek-harness)) profile bundle for [xerj](https://github.com/xerj-org/xerj). It inserts one `@deepseek-ai/dsh-mcp-client` row that runs `xerj mcp` over stdio. Tools register as `mcp__xerj__*`.

This bundle is not yet tested on DSH.

## Prerequisites

- Node.js `>=22.12.0`. `dsh plugin` forwards to [pnpm](https://pnpm.io/installation).
- The `xerj` binary on `PATH` (`command: xerj` in the patch). The `sh` installer puts it in `~/.local/bin` and does not edit `PATH`.
- A xerj node already running. `xerj mcp` does not start one. The patch's `XERJ_URL` is `http://localhost:9200`.

## Install

The commands pin `@deepseek-ai/dsh@0.2.1-rc.3`, the release that depends on `@deepseek-ai/dsh-mcp-client@0.2.1-rc.3` (the version in `package.json`). `npm view @deepseek-ai/dsh-mcp-client version` still reports the `latest` tag `0.0.1-rc.1`; the published line this bundle pins is the `next` tag `0.2.1-rc.3`. With that `dsh` on `PATH`, drop the `npx` prefix.

```sh
git clone https://github.com/xerj-org/xerj.git
cd xerj
npx @deepseek-ai/dsh@0.2.1-rc.3 plugin --profile web add link:./dsh
```

Replace `web` with the profile name. A profile that does not exist yet is created. Restart the profile (`npx @deepseek-ai/dsh@0.2.1-rc.3 web`, or relaunch) so the row loads.

Remove: `npx @deepseek-ai/dsh@0.2.1-rc.3 plugin --profile web remove xerj-dsh`.

## Override

DSH strips credential-like environment variables from the process it spawns, so a shell export of `XERJ_AUTH` does not reach `xerj mcp`. When the node is not running `--insecure`, put `XERJ_AUTH` in the profile patch `$DSH_HOME/profiles/<profile>/cordis.patch.yml`. An override replaces the whole config, so restate every field:

```yaml
- id: mcp-xerj
  config:
    serverName: xerj
    transport: stdio
    command: xerj
    args:
      - mcp
    env:
      XERJ_URL: "http://localhost:9200"
      XERJ_AUTH: "ApiKey <key>"
    cwd: ""
```

The key is `<data-dir>/admin.key`. `serverName` must stay unique among live `@deepseek-ai/dsh-mcp-client` rows. To point `command` at an absolute path instead of `xerj` on `PATH`, use the same full-config override.
