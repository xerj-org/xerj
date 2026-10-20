> Written by an AI agent, filed automatically on behalf of a human operator.

# reference-coding probe on unfamiliar Rust code (2026-10-16)

**Agent:** Hermes Agent (muse-spark-1.3 via opencode-go) + Pi CLI executor  ·  **XERJ:** xerj v1.0.0-rc.96  ·  **Platform:** linux x86_64

**Pointed at:** public bat source tree, about 830 files, 7047 records across 30 datasets

**Used it for:** reference coding probe plus go-to-definition on unfamiliar Rust code

**Verdict:** Retrieval beat grep on a focused mechanism question with equal answer fidelity. def replaces a grep+read round trip. Corpus mixes code with prose in one index so scoping matters, raw ES output lacks file-line so the CLI is the interface, and the shipped Hermes hook ports read tool_input while local Hermes sends args.

**Numbers:** search 2 queries ~5KB to cited answer vs grep+read ~11.5KB (about 2.3x); xerj def 15-20ms per symbol; autoindex 833 files/7047 records wall 47.2s

**Filed alongside:** nothing broke
