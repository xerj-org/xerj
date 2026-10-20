# Search over a Spanish-language R package, measured against rg (2026-10-12)

**XERJ:** xerj v1.0.0-rc.95  ·  **Platform:** Pop!_OS 22.04 x86_64, prebuilt musl binary

**Pointed at:** a snapshot of an R package (sebollin/lupa at a705445): 587 files, ~88k lines of R and ~74k of tests, with comments and docs in Spanish.

**Used it for:** autoindex + `xerj search` / `xerj def` against `rg`, on four questions whose answers were written down and verified before anything ran. Direct queries only, no agent loop.

**Verdict:** symbol lookup is good: `xerj def` returned the exact definition and signature in about 300 bytes. Natural-language
search did not work on this corpus. Three questions phrased in Spanish returned no answer (one partial: right file, no function or
line). Two unrelated ones returned the same five hits, three of them test helpers named `de`. Whole-file hits also print no line, so
citing `ax_path:line` meant opening the file. On code whose prose is not English, I would use `xerj def`, not `xerj search`.

**Numbers:** `xerj autoindex` → wall=9.2s, files=587, records=12371, code_files=476; data dir 78 MB while running, 22 MB after stop;
server RSS 176 MB. Against the written answers: `rg` found every code location in all four, `xerj search` none of the three
natural-language questions, `xerj def` both definitions it was asked for. Output size: 16,645 bytes over 8 xerj queries vs 76,333 over
7 rg queries (55k of it from one over-broad pattern). `ss -tnp` showed no outbound connection at the one moment checked.

**Filed alongside:** #1364 (Spanish function words drive `xerj search` ranking), #1365 (whole-file hits print no line)
