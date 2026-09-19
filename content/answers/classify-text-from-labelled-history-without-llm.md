---
title: "Can I route or spam-filter text without an LLM call?"
h1: "Can I route tickets or filter spam from labelled history, without an LLM call?"
description: "Index labelled examples and classify new text by its nearest neighbours. Measured: 0.933 accuracy on 77-way intent routing, 0.983 on SMS spam, no LLM or API call."
slug: "classify-text-from-labelled-history-without-llm"
cluster: "Decisions as retrieval"
question: "Can I classify text from labelled history without calling an LLM?"
intent: "informational"
published: "2026-09-18"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, index the user's labelled examples into one XERJ index with the label in a keyword field, classify each held-out item by a rank-weighted vote over its 10 nearest neighbours, and report accuracy, the share of items decided at confidence 0.8 or higher, and the accuracy on that share."
commands:
  - cmd: "xerj --insecure --data-dir ./decisions-data --embed-mode neural"
    note: "Start a throwaway node. The neural flag is needed for the MiniLM and hybrid rows only. The BM25 row uses no embedder and works on a default node."
  - cmd: "curl -so sms.tsv https://raw.githubusercontent.com/justmarkham/pycon-2016-tutorial/master/data/sms.tsv"
    note: "Download the public SMS spam dataset used for the yes-or-no run."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/sms/_search' -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"text\":\"you have won a free prize call now\"}},\"size\":10,\"_source\":[\"label\"]}'"
    note: "Fetch the 10 nearest labelled examples for one new message. The vote over their labels is the decision, and the winning share is the confidence."
links_out:
  - "store-agent-memory-without-llm-call"
  - "hybrid-search-quality-measured-ndcg"
  - "rerank-search-results-calibrated-judge"
  - "do-search-embeddings-help"
evidence:
  - claim: "Banking77, 77-way intent routing, 10,003 labelled examples indexed, 1,000 held-out test items, k = 10: accuracy 0.819 for BM25, 0.933 for MiniLM neighbours and 0.937 for hybrid RRF; expected calibration error 0.089, 0.012 and 0.052; 50.6%, 86.9% and 79.2% of items decided at confidence 0.8 or higher, at accuracy 0.996, 0.979 and 0.989; 2.2, 8.6 and 12.4 ms per item."
    source: "benchmarks/decisions-as-retrieval/results/run-2026-09-18.txt"
  - claim: "SMS spam, yes-or-no detection, 4,000 labelled examples indexed, 1,000 held-out test items, k = 10: accuracy 0.983 for BM25, 0.982 for MiniLM neighbours and 0.989 for hybrid RRF; spam F1 0.944, 0.940 and 0.962; expected calibration error 0.017, 0.009 and 0.015; 0.9, 74.5 and 67.2 ms per item."
    source: "benchmarks/decisions-as-retrieval/results/run-2026-09-18.txt"
  - claim: "The run was made on 2026-09-18 with xerj v1.0.0-rc.74 and --embed-mode neural using all-MiniLM-L6-v2 on CPU, with a seeded shuffle for the held-out split, on a machine that was also compiling."
    source: "benchmarks/decisions-as-retrieval/README.md"
faq:
  - q: "Can I classify text without calling an LLM?"
    a: "Yes, when you have labelled history. Index the labelled examples, fetch the 10 nearest for each new item, and take a rank-weighted vote over their labels. No model call is made per decision."
  - q: "How accurate is nearest-neighbour classification on a real dataset?"
    a: "On Banking77, a 77-way intent dataset, XERJ measured 0.819 with BM25 and 0.933 with MiniLM neighbours. On SMS spam it measured 0.983 with BM25 alone. The MiniLM figures need `--embed-mode neural`."
  - q: "Can I trust the confidence value?"
    a: "In this run, yes. Expected calibration error was 0.012 on Banking77 and 0.009 on SMS spam for the MiniLM arm: averaged over confidence bins, the reported confidence and the observed accuracy differed by about one point."
  - q: "Does this work with no labelled examples?"
    a: "No. This is not zero-shot. A new category or a new policy has no history to vote from, and that case needs a judge model or a person."
  - q: "Do I need a neural embedder for this?"
    a: "Not for spam. BM25 alone reached 0.983 on SMS spam with no embedder at all. For 77-way intent routing the neural arm added about 11 points of accuracy over BM25."
  - q: "Did XERJ compare this against an LLM judge?"
    a: "No. XERJ did not run any judge model on these datasets and claims nothing about how one would score."
---

**TL;DR** — If you already have labelled examples, you can route and filter text with no LLM or API call. Index the examples, fetch the 10 nearest for each new item, and vote. Measured: 0.933 accuracy on 77-way intent routing and 0.983 on SMS spam. The 0.933 figure needs `--embed-mode neural`, which runs one local MiniLM embedding per item; the spam figure is plain BM25, with no model of any kind.

## The method

A judge model answers typed questions: pick one of N, or yes-or-no with a probability. Where a team already has labelled history, a search index can answer the same shape of question.

1. Index every labelled example, with the label in a keyword field.
2. For a new item, fetch its 10 nearest neighbours.
3. Take a rank-weighted vote over the neighbours' labels.
4. Report the winning label, and its share of the vote as the confidence.

No model runs per decision on the BM25 arm. On the neural arms one local embedding runs per item, on the node, with no API call.

## Intent routing: Banking77

Banking77 is a public dataset of banking support messages with 77 intent labels. The run indexed 10,003 labelled examples and classified 1,000 held-out items.

| Arm | Accuracy | Calibration error | Decided at confidence 0.8 or higher | Accuracy on that share | ms per item |
| --- | --- | --- | --- | --- | --- |
| BM25, no model of any kind | 0.819 | 0.089 | 50.6% | 0.996 | 2.2 |
| MiniLM neighbours | 0.933 | 0.012 | 86.9% | 0.979 | 8.6 |
| Hybrid RRF | 0.937 | 0.052 | 79.2% | 0.989 | 12.4 |

## Spam detection: SMS spam

The SMS spam dataset is a public yes-or-no task. The run indexed 4,000 labelled messages and classified 1,000 held-out ones.

| Arm | Accuracy | Spam F1 | Calibration error | ms per item |
| --- | --- | --- | --- | --- |
| BM25, no model of any kind | 0.983 | 0.944 | 0.017 | 0.9 |
| MiniLM neighbours | 0.982 | 0.940 | 0.009 | 74.5 |
| Hybrid RRF | 0.989 | 0.962 | 0.015 | 67.2 |

Plain BM25 over 4,000 examples reached 0.983 in under a millisecond per message. The neural arm did not improve accuracy here, and it cost far more time.

## Which embedder these numbers need

The BM25 rows use no embedder, so they hold on a default XERJ node. The MiniLM and hybrid rows were measured with `--embed-mode neural` and the all-MiniLM-L6-v2 model on CPU.

XERJ's default embedder is lexical feature hashing. It has no model in it. Do not read the MiniLM or hybrid rows as what a default node scores.

## The confidence gate is the point

An expected calibration error of 0.012 means the reported confidence is usable: averaged over confidence bins, the gap between the confidence the vote reported and the accuracy observed at that confidence was about one point in this run.

That makes the confidence a gate. On Banking77, the MiniLM arm decided 86.9% of items at confidence 0.8 or higher, and was right on 0.979 of them. Only the remaining items need a judge model or a person.

This is the economics of a two-stage system. A free local stage takes the bulk, and the paid stage sees only what is uncertain.

## When to call a judge model instead

Call a judge model when there is no labelled history. This method is not zero-shot. A new policy or a new category has nothing to vote from, and a judge model needs no examples.

Call one also for the low-confidence share. XERJ's `rerank` search stage can send documents to an external judge when an operator has set a provider key. XERJ did not run any judge model on these two datasets. It claims nothing about how one would score on them.

## What this run does not show

Two public datasets, one run each, 1,000 held-out items per dataset, k fixed at 10. The held-out split uses a seeded shuffle. The latency was measured on a machine that was also compiling, so treat it as an upper bound.

The gap between the two datasets' latencies is query length. Short banking messages embed in a few milliseconds, and longer SMS bodies take tens.
