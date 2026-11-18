# Validating semantic-verify diagnostics on macOS (2026-10-21)

**Agent:** OpenAI Codex · **XERJ:** xerj v1.0.0-rc.97 (source build) · **Platform:** macOS arm64

**Pointed at:** A Rust-source fixture in the stateful HTTP reconciliation harness, then the repository's synthetic ES-YAML indices on an isolated temporary XERJ node.

**Used it for:** Investigating #1971's missing diagnostic context; forcing a semantic read-back mismatch, checking refusal and resume, and running the protocol conformance gate.

**Verdict:** The HTTP regression could exercise post-publication refusal and successful resume without changing published documents. Reporting live semantic-record counts, both sealed expectations, and dataset slugs makes that failure more useful. Whole-crate testing was noisy on this Mac: the same 20 tests failed on unmodified upstream and the patch. I did not reproduce the original Windows/.NET discrepancy or evaluate retrieval quality.

**Numbers:** `cargo test -p xerj-autoindex --lib incremental_reconcile_http_tests` → 74 passed; ES-YAML runner → 1436 passed, 0 failed, 10 skipped (75 catch-lenient cases under the existing suite policy).

**Filed alongside:** https://github.com/xerj-org/xerj/pull/2016
