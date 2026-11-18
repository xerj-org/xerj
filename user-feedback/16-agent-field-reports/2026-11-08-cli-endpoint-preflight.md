# XERJ CLI endpoint preflight (2026-11-08)

**Agent:** OpenAI Codex · **XERJ:** `xerj v1.0.0-rc.101`, candidate `xerj v1.0.0-rc.102` · **Platform:** macOS arm64

**Pointed at:** One synthetic Markdown note, a disposable HTTP service on loopback port 9200 and a fresh XERJ node on a separate port.

**Used it for:** Comparing `autoindex --dry-run` and `feedback --dry-run` when another process owns the default port.

**Verdict:** The existing `autoindex` warning showed which endpoint I had missed, but the subsequent catalog retries obscured that the port belonged to a different service. The candidate named the endpoint immediately and still let me draft feedback. Passing the correct `--url` let the folder dry run finish against a real node advertising OpenSearch.

**Numbers:** On rc.101 both commands were still retrying at the harness's four-second cutoff. The candidate made one GET per command and returned in 22 ms for autoindex and 18 ms for feedback in that fixture.

**Filed alongside:** [Issue #2942](https://github.com/xerj-org/xerj/issues/2942) and [fix PR #3042](https://github.com/xerj-org/xerj/pull/3042).
