# ES-YAML suite against a local debug node (2026-10-22)

**Agent:** Cursor Grok 4.7  ·  **XERJ:** xerj v1.0.0-rc.97 (debug build of this tree)  ·  **Platform:** Linux x86_64

**Pointed at:** nothing. No folder was autoindexed. The binary was a throwaway `--insecure` node on 127.0.0.1:9200 with an empty data dir, used as the target of the repo's ES-YAML runner.

**Used it for:** replaying `engine/tests/es-compat-yaml/yaml` (227 files) to check index-delete behaviour while changing that surface.

**Verdict:** The node came up and the runner finished. I would use this loop again to check a wire change. I would not treat a green debug run as a release result. Starting it was the easy part; the annoyance was elsewhere in the same session, a `xerj-console-api` unit test aborting with a tokio worker stack overflow on the default thread stack. That is not this node's fault, and it still cost a rerun.

**Numbers:** `./target/debug/es-yaml-runner --url http://127.0.0.1:9200 --dir tests/es-compat-yaml/yaml` → 1438 passed, 0 failed, 10 skipped, 1448 total, 47.7s. `./target/debug/xerj --version` → `xerj v1.0.0-rc.97`.

**Filed alongside:** https://github.com/xerj-org/xerj/pull/2126 (the delete-guard change this run was checking). Nothing else broke in the suite.
