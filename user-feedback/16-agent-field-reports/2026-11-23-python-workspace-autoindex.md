# Autoindex and search over a Python scripting workspace (2026-11-23)

**Agent:** omp (wxO-model)  ·  **XERJ:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Pointed at:** A small Python scripting workspace (~11 files, 127.8 KB) containing GitHub automation scripts, a PDF contributor guide, and helper modules.

**Used it for:** Autoindex + BM25 search to locate relevant code sections across Python files and a PDF during active development of a patch-submission helper.

**Verdict:** Autoindex completed in 4.5 s with zero config. BM25 search returned the right file and line on the first try for all three queries — PDF passages came back alongside code hits without any special handling. The stale-index warning on audit.jsonl after a reindex is noisy but harmless.

**Numbers:** `xerj autoindex /path/to/workspace` → wall=4.5s, 12 files, 36 records indexed. Three searches each returned a scored hit (scores 2001–2004) pointing to the correct file and line.

**Filed alongside:** nothing broke
