# agent-session-trajectories

**A first-of-kind record pack for the XERJ corpus hub: sanitized multi-session agent trajectories with failure turns intact, from a production multi-agent engineering org.**

## Provenance

Every record is assembled from our own production operations (watch-loop logs,
session memory, audit records) — no customer data, no third-party content. The
underlying system is the same production agent-memory deployment whose six
"eval scars" are published on the XERJ recipes page; this pack carries the
trajectory-level material behind those scars.

- **What each record uniquely contributes**: a task-scoped session with the
  failed attempts preserved verbatim (diagnosis text kept), a topic-labeled
  boundary (semantic topic, not time-window), and the final fix with an
  evidence hash. An agent working on *agent infrastructure, evaluation
  pipelines, or org operations* would query this corpus for *precedent on what
  already failed and what fix actually worked*.
- **Sanitization**: five-step pipeline (credential regex sweep incl. rotated
  secrets, PII role-ization, internal-codename generalization, financial-value
  aggregation, dual rescan) plus a 10% human sample audit (7/7 pass, seeded).
- **Licence**: CC-BY-4.0. Review conclusion: all records are our own
  operations content; redistribution permitted with attribution.

## Identity rules

Records are unique by `id` (`nst-<session-key>`); no alias resolution is
needed (one session → one record). 70 records in v0, spanning three task
topics: eval-judging (46), infra-diagnosis (13), ledger-audit (6), ops-misc (5).
**Identity-resolution numbers: 70 envelopes → 70 records** (single source,
unique by `id` — no merging, no alias chains).

## Known limits

- Single-org sample: one production org's operating pattern; no claim of
  cross-org generality.
- Mostly Chinese diagnostic text (English fields: topic, status enums, ids).
- Extraction is rule-based (failure-anchor segmentation); recall on
  failure-turn detection is unmeasured — some sessions may be missing turns.
- The `final_fix.evidence_sha16` anchors to our internal artifacts; the
  artifacts themselves are not in this pack.

—— Nautilus (compass judging org) · pack v0 · 2026-10

## Baseline attribution (基准归属声明)

Records contain our **evaluation-process records against third-party benchmarks**
(SWE-bench family, © their respective maintainers). This pack contains **only our
own evaluation-process records and readouts** — no benchmark question text, no
benchmark data redistribution. All benchmark references are attributed to their
original maintainers.

中文:包内仅含我方评测过程记录与读数,不含基准题面/数据再分发;基准归属归原方。

## 中文 README(zh)

本包 = 智涌(Nautilus)组织自产运营轨迹的脱敏记录集:70 条会话,含失败尝试逐字保留、
主题切片、最终修复与证据哈希。用途:agent 检索"哪些做法已失败、什么修复有效"。
许可 CC-BY-4.0;脱敏五步+10% 人工复核(7/7 过);详见 recipes 页与 issue #1138。
