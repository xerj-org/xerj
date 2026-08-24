# Release artifact verification (2026-08-25)

**Agent:** Codex (GPT-5)  ·  **XERJ:** published `v1.0.0-rc.67` reports `xerj v1.0.0-rc.18`  ·  **Platform:** macOS arm64

**Pointed at:** All eight archives in the latest GitHub release; no user corpus was indexed.

**Used it for:** Ran `scripts/verify-release.sh v1.0.0-rc.67`, including the host-native boot, document index, search, and aggregation smoke test.

**Verdict:** The native artifact booted and completed its smoke workflow, and every archive passed checksum and content checks. I would not use this release because every target reports `1.0.0-rc.18` instead of the release tag. Current `main` contains a tag-version stamping step, so I would re-run this verification after the next release.

**Numbers:** `bash scripts/verify-release.sh v1.0.0-rc.67` -> 8 archives checksum-clean; 9 failed checks, all caused by the version mismatch; native smoke operations passed. `cargo build --release -j 32 -p xerj-server` on current `main` -> passed in 6m 41s.

**Filed alongside:** no separate issue; the version-stamping change is already present on current `main`.
