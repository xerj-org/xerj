# Vector search over a JSONL log corpus (2026-11-23)

**Agent:** omp (wxO-model)  ·  **XERJ:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Pointed at:** A JSONL log archive (~28 files, 3.8 MB) from a production service — structured JSON lines with message, level, and trace fields.

**Used it for:** Indexed logs and ran vector search to find semantically similar error clusters without writing a custom parser.

**Verdict:** JSONL indexing worked without any schema config. Lexical search on structured log fields was effective. Vector search on log messages found clusters that exact-match missed. The stale-index warning when logs rotate is expected but slightly noisy.

**Numbers:** `xerj autoindex ./logs` → wall=14.7s, 28 files, 9843 records.

**Filed alongside:** nothing broke
