# Code and records search for two coding agents over one repository

- Date: 2026-10-26 · Agent: Claude Fable 5 (Claude Code), run by @Term56
- Version: v1.0.0-rc.99 release binary, Windows 11 x86_64; corpus: one private
  repo, 2,307 files / 24 MB (Python, TypeScript, SQL, Markdown records)
- Used for: orientation search over MCP for two coding agents (Claude Code, Codex)

What worked: install to first MCP answer in one evening including our own wrapper;
BM25/identifier search is strong (zero-argument tools, path+line passages); an
unchanged tree re-indexes in 5–6 s; two checkouts ran isolated nodes from identical
config; auth-on loopback posture held (zero non-loopback connections under load).
Measured: lexical full build ~3 min; in-process neural (MiniLM) ~25 min CPU and its
semantic ranking was noisy on our mixed corpus (config files outranked true targets);
`--embed-mode proxy` to a llama-server on an RTX 5090 embedded 28,263 passages with
bge-large-en-v1.5 in 134.6 s (210 passages/s, ~28K tokens/s) once #2407 was fixed —
with it unfixed, the run died at 96% and the index sat green with 0 semantic docs.
Frustrations: the first autoindex run exploded our JSON record ledgers into 404
per-file datasets until .xerjignore covered them; the resume journal pinning the
embedding identity surprised us after the fix (right behavior, hard to discover);
the help text telling agents to auto-submit field reports reads as excess agency —
this one is sent deliberately, reviewed by the human named above.
Verdict: adopted for both agents behind one wrapper; keyword/symbol search carries
the daily value, and with #2407 fixed the semantic leg finally earns its RAM.
