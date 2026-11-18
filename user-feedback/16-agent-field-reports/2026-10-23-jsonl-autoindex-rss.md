# Autoindexing a small-record JSONL while measuring node RSS (2026-10-23)

**Agent:** Cursor Grok 4.7  ·  **XERJ:** xerj v1.0.0-rc.98 (debug build)  ·  **Platform:** Linux 6.12.94+ x86_64

**Pointed at:** one JSONL of synthetic log lines from `benchmarks/best-of-jsonl-search-2026/gen_corpus.py` (5,000 rows, 972,808 bytes, then 20,000 rows, 3,893,555 bytes). Lexical feature-hash; `--embed-mode neural` was not set.

**Used it for:** `xerj autoindex` of that file (`--insecure --no-graph --prefix bx`) against a local node, to measure RSS for issue #1940.

**Verdict:** The command finished cleanly and the count matched the file. I would not autoindex a few hundred thousand of these rows on a 16 GB machine on the unfixed binary: 5,000 records of about 195 bytes peaked at VmHWM 923,084 kB from an idle 112,088 kB. The bar and the `xerj-done` line were accurate. The memory was the product, not the progress UI.

**Numbers:** main, 5k: `xerj-done wall=27.7s records=5000`, VmHWM 923084 kB, verify `hydrate+corpus=2006ms`. After packing the embedding, same file: wall=22.0s, VmHWM 398516 kB from idle 105264 kB. Fresh 20k: wall=100.0s, count 20000, VmHWM 819100 kB, end VmRSS 327640 kB, `hydrate+corpus=3ms` on the post-flush read-back.

**Filed alongside:** https://github.com/xerj-org/xerj/pull/2240 (Fixes #1940)
