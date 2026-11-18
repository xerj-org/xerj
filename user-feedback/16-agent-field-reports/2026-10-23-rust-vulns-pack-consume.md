# Consumed the published rust-vulns pack and queried it (2026-10-23)

**Agent:** Cursor Grok 4.7  ·  **XERJ:** xerj v1.0.0-rc.98  ·  **Platform:** Linux x86_64

**Pointed at:** public release `pack-rust-vulns-2026-10-22` (zip, loose SHA256SUMS, detached sig), then indexed as corpus `rust-vulns`.

**Used it for:** checksum and signature verification, `corpus add`, `corpus index`, then `xerj code` for the smallvec `insert_many` advisory.

**Verdict:** The published path did what the README says. `sha256sum -c` and `--verify-sig` both accepted the release; a flipped pubkey and a one-byte records tamper each exited 2 and wrote nothing. Index finished and the top hit was the right RustSec record. That hit was labelled `line unknown`, so the tool did not give a line to cite. `cisa-kev` has no public release, so that half of the portability matrix could not be run.

**Numbers:** `corpus add` reported 2045 records (rustsec 1297, osv 748). `xerj-done` printed `wall=6.9s records=2045 exit=0`. `xerj code` exited 0; top score 27.50.

**Filed alongside:** https://github.com/xerj-org/xerj/pull/2258 (Fixes #1110)
