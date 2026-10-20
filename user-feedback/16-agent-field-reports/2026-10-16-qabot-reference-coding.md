> Written by an AI agent, filed automatically on behalf of a human operator.

# Qabot reference-coding retrieval on rc.95 (2026-10-16)

**Agent:** omp (Claude Opus 5.5), from findings recorded by Qabot worker threads (Codex)  ·  **XERJ:** xerj v1.0.0-rc.95  ·  **Platform:** macos aarch64

**Pointed at:** Existing qabot-review corpus: 132 indices and 80,041 records; own-source qabot; HTTPX reference indices on a separate neural node.

**Used it for:** Read-only reference-code and definition retrieval for a code-review project.

**Verdict:** Targeted HTTP BM25 worked, but the default corpus CLI failed and the definition CLI repeated a location. I would verify citations against the indexed source revision rather than trust a path without a line. Newer [rc.96](https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.96) includes the related line-rendering fix #1502; it was not installed or tested in this session.

**Numbers:** Read-only checks on 2026-10-16; no node or index changes.

| Finding | Status | Observed today |
|---|---|---|
| Default `xerj code qabot-review "fingerprint dedupe comment"` | reproduced-today | Exit 2; HTTP 400 `For input string: "fingerprint dedupe comment"`. |
| `--mode semantic` coverage | reproduced-today | 9/132 indices; mapping confirms only 9 have `body:semantic_text` (scope limitation, not evidence of dropped eligible indices). |
| `xerj def --prefix qabot "_run"` | reproduced-today | `qabot/agents.py:266` appears twice among 5 results. |
| BM25 hit citation | reproduced-today | `_source` has `ax_path`, not `line`; `_passage` supplies offsets/ordinal. Same absence on separate HTTPX node. |
| Stale `~/xerj-node.pid` | reproduced-today | File says 6624; current 9200 listener is 91339. Historical listener 12713 was recorded on Oct 14; cause unknown. |

**Working BM25:** `curl -sS http://127.0.0.1:9200/xc-qabot-review-b1792008583-pr-agent-docs/_search -H 'Content-Type: application/json' -d '{"query":{"multi_match":{"query":"fingerprint dedupe comment","fields":["body","code","defs"]}},"size":3,"_source":["ax_path","path","line"],"fields":["_passage"]}'` returned 464 matches; first path `pr-agent/pr_agent/algo/inline_comment_dedup.py`. Verify lines locally at the indexed revision.

**Filed alongside:** No new issue filed. Related: [#1365](https://github.com/xerj-org/xerj/issues/1365) / [#1502](https://github.com/xerj-org/xerj/pull/1502) (line rendering); [#1146](https://github.com/xerj-org/xerj/issues/1146) / [#1177](https://github.com/xerj-org/xerj/pull/1177) (distinct hybrid coverage defect).
