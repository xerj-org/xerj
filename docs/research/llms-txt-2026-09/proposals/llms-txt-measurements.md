# llms.txt measurements

Produced by `scripts/measure_llms_txt.py`; do not edit by hand. Definitions are in the script's docstring. Per-file rows (URL, status, bytes, SHA-256, fetch time, every measure) are in `data/llms-txt-manifest.json`. Third-party bodies are not committed.

- URLs fetched: **132** (2026-09-19T20:08:19Z – 2026-09-19T20:28:58Z UTC). Kept: **128** (127 peers + XERJ). Dropped: **4** (duplicate).
- Generated index files change daily; re-run before quoting a number.

| Measure | min | p25 | median | p75 | p90 | max | XERJ today | XERJ percentile | Proposal | Proposal percentile |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Bytes | 662 | 7,866 | 22,486 | 48,355 | 102,795 | 2,645,454 | 40,064 | 68th | 17,155 | 41st |
| Lines | 14 | 89 | 189 | 410 | 796 | 61,194 | 234 | 56th | 162 | 44th |
| Prose bytes | 0 | 167 | 388.5 | 1,191 | 3,568 | 869,417 | 9,200 | 96th | 3,832 | 91st |
| Unfenced lines over 400 characters | 0 | 0 | 0 | 1 | 4 | 240 | 23 | 98th | 0 | 64th |
| Prose lines over 400 characters (stricter) | 0 | 0 | 0 | 0 | 2 | 199 | 10 | 98th | 0 | 81st |
| Longest unfenced line (characters) | 122 | 265 | 348.5 | 431 | 658 | 5,500 | 1,124 | 98th | 340 | 47th |
| Obligation words | 0 | 0 | 0 | 3 | 7 | 452 | 21 | 97th | 12 | 95th |

## Counts over the kept peer files

- An install-like command anywhere in llms.txt (regex `INSTALL_RE`): **28 of 127**.
- At least one fenced code block: **17 of 127**.
- Neither a fenced block nor an install-like command: **94 of 127**.
- `claude mcp add` in llms.txt itself: **2 of 127** (Haystack, Tavily).
- `mcpServers` in llms.txt itself: **2 of 127** (Gemini-CLI, Haystack).
- `npx skills add` in llms.txt itself: **3 of 127**.
- A `## Optional` heading: **38 of 127**. Of those, files with "required", "must", "owe" or "obligat…" inside that section: **0**. XERJ's `## Optional` section contains **2**.

## XERJ today and the proposal

| | XERJ today | Proposal |
|---|---:|---:|
| Bytes | 40,064 | 17,155 |
| Lines | 234 | 162 |
| Prose bytes | 9,200 | 3,832 |
| Unfenced lines over 400 characters | 23 | 0 |
| Prose lines over 400 characters (stricter) | 10 | 0 |
| Longest unfenced line (characters) | 1,124 | 340 |
| Obligation words | 21 | 12 |
| Bytes before the first install command | 1,788 | 2,182 |
| Fenced code blocks | 0 | 4 |
| `claude mcp add` lines | 0 | 1 |
| `mcpServers` occurrences | 0 | 3 |

## Highest obligation-word counts among files under 100 KB

| File | Obligation words | Bytes |
|---|---:|---:|
| Stripe — https://docs.stripe.com/llms.txt | 43 | 92,159 |
| XERJ — https://xerj.org/llms.txt | 21 | 40,064 |
| Milvus — https://milvus.io/llms.txt | 18 | 81,998 |
| Open-WebUI — https://docs.openwebui.com/llms.txt | 12 | 86,617 |
| Together — https://docs.together.ai/llms.txt | 11 | 64,561 |
| Browser-Use — https://docs.browser-use.com/llms.txt | 8 | 42,557 |
| Meilisearch — https://www.meilisearch.com/docs/llms.txt | 8 | 96,235 |
| Mem0 — https://docs.mem0.ai/llms.txt | 8 | 43,564 |

## Size histogram (kept files)

```text
   0 –   5 KB   20  ####################
   5 –  10 KB   17  #################
  10 –  20 KB   24  ########################
  20 –  40 KB   25  #########################
  40 –  80 KB   22  ######################   <- XERJ 40,064
  80 – 200 KB   12  ############
      > 200 KB    8  ########
```
