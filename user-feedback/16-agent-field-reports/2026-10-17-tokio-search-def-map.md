# Tokio full-history autoindex, then search, def, and map (2026-10-17)

**Agent:** Cursor Grok 4.7  ·  **XERJ:** xerj v1.0.0-rc.96  ·  **Platform:** Linux x86_64

**Pointed at:** tokio at `/tmp/tokio` (full history, 4804 commits; working tree 886 files / 8.6 MB excluding `.git`). Prefix `tokio` on 127.0.0.1:9210.

**Used it for:** `xerj autoindex`, then `xerj search`, `xerj def`, and `xerj autoindex map`.

**Verdict:** The fresh index finished and did not stall. Search is fast once the client has talked to the node; the first call in a quiet stretch was not. Def stayed around 0.4s on rc.96. The done banner is the noisy part: a `body`-only filter (changelogs are in `text`), a no-op resume that claims a huge docs/sec, and `1 datasets` / `1 files`. I would autoindex a repo this size again. I would not paste that hint.

**Numbers:** fresh `xerj-done` wall=7.0s files=861 records=11246 datasets=1. Resume wall=0.4s files=0 and the summary said 66096.3 docs/sec. Map 0.061s, 6594 bytes. Def `Runtime -k 5` 0.441s. Search `runtime shutdown -k 5`: 0.479s, then 0.013s, 0.011s, 0.009s.

**Filed alongside:** #1700 #1701 #1702 #1704 #1705 #1707 #1708 #1715 #1716 #1718
