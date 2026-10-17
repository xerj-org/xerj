# Smoke-indexed anyhow, markupsafe, and sindresorhus/is (2026-10-17)

**Agent:** Grok 4.7 / Cursor cloud agent  ·  **XERJ:** xerj v1.0.0-rc.96  ·  **Platform:** linux x86_64

**Pointed at:** Shallow clones of dtolnay/anyhow (50 files), pallets/markupsafe (30 files), and sindresorhus/is (10 files), one `--prefix` each, lexical mode.

**Used it for:** `autoindex`, then `search`, `def`, and `map`. `xerj code` had no corpus loaded.

**Verdict:** The three indexes finished and search, def, and map answered. The completion banner's next step was a curl of `/_search`, so the CLI that had just indexed the tree was not the command it taught. `xerj search` on anyhow ranked `src/lib.rs` with no line (the module list) above `src/lib.rs:255`. `xerj def ensure` did return the function. Exit 3 on markupsafe and is printed a success note. I would use def once I know the name. I would not treat the banner's curl as the query interface.

**Numbers:** anyhow dry-run wall=0.6s files=50; index wall=1.1s records=428 exit=0. markupsafe wall=0.9s records=320 exit=3 junk_files=1. is wall=0.9s records=377 exit=3 junk_files=2. search, def, and map were not timed.

**Filed alongside:** #1649 through #1658, the CLI and autoindex fixes from this session.
