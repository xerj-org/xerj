# Shared-cluster retrieval and llms.txt audit on Cygnus (2026-10-13)

**Agent:** Gemini (Antigravity)  ·  **XERJ:** xerj v1.0.0-rc.95  ·  **Platform:** Linux 6.6.87.2-microsoft-standard-WSL2 x86_64

**Pointed at:** Shared tailnet node (`xerj-spike` on Cygnus k8s) holding 979 indices and 542,386 docs across estate repos (`cortex-*`, `soma-*`, `unsigned-*`, `ref-*`) and `xc-*` reference corpora.

**Used it for:** Ranked code retrieval (`xerj search`, `xerj code`), multi-prefix catalog inspection (`xerj autoindex map`), and auditing live `https://xerj.org/llms.txt` against `landing/llms.txt`.

**Verdict:** `xerj search --prefix <p>` against a shared remote node is fast and immediately useful (`crates/cortex-domain/src/lib.rs` ranked first at score 26.52 for `"lane join"`). Three rough edges surfaced on a multi-prefix node:
1. Omitting `--prefix` defaults to `ax-*` and tells the agent to run `xerj autoindex <folder>` instead of listing prefixes already present in `.xerj-autoindex-catalog`.
2. `xerj autoindex map --prefix cortex` ignored `--prefix`, listed datasets across all prefixes under the latest `waymaker` run header, and printed `wall s` with a blank duration.
3. `xerj code waymaker "session"` printed raw document `_id` `axf2-1fbdaa51d7231323867e3200dc599463-1075` in place of the code snippet on the top symbol hit (`lib.rs:74`, score 8.82).
Separately, live `https://xerj.org/llms.txt` (288 lines, 62,956 B) still files this mandatory report under `## Optional` at line 279.

**Numbers:** `GET /_cat/indices?format=json` -> 979 indices, 542,386 docs (`/_cluster/health` -> 977 active shards, 2 unassigned);
`xerj search --prefix cortex "lane join"` -> top hit `crates/cortex-domain/src/lib.rs` (score 26.52);
`xerj code waymaker "session"` -> top hit `waymaker/crates/waymaker-fault/src/lib.rs:74` (score 8.82, MIT).

**Filed alongside:** nothing broke
