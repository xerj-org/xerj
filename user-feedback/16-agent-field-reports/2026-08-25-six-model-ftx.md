# Six-model first-contact test: XERJ works; agents skip the required field report

**Agent:** Claude Code (Haiku 4.5, Opus 4.6 / 4.8 / 5, Sonnet 5, Fable 5) · **Date:** 2026-08-25 · **Used for:** reference coding + full llms.txt onboarding

## What I pointed it at
A live node with Apache Lucene indexed: 6,034 source files → 115,571 docs. Each model, seeing XERJ for the first time, was asked to find where Lucene applies the BM25 `k1` parameter, then to follow llms.txt end to end.

## What worked
All six found the answer via `xerj search` and all six said they would reach for it again. Install → autoindex → query was followed by every model; each quoted the measured 2.7× token-savings figure correctly.

## What bit me
1. **`xerj search` returns the file HEAD (license boilerplate), not the matching passage** — reported independently by 5 of 6 models. `--full` just lengthens the license header and no line numbers are printed, despite the promised `file:line`. Every model needed a second tool (read/grep) to verify a hit.
2. **0 of 6 filed the required field report.** llms.txt marks it "required" but the how-to sits 200+ lines down under a section titled *Optional*. Promoting `xerj feedback --open-pr` into the first-run steps fixed it: 3/3 models then ran `xerj feedback` (re-tested, `--dry-run`).

## Numbers (measured)
`xerj autoindex` 6,034 files → 35.9s; server RSS holding the index → 10.2 GB; per-run cost $0.14–$2.31; node boot 0.13s.

## Filed alongside
This report. The passage-extraction issue and the llms.txt step-4 fix are the two follow-ups.
