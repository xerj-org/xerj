# Agent-simulation exam — /tmp/xerj-discover/corpus (ground truth recorded BEFORE any agent run)

Ground truth sources: the secret manifest /tmp/xerj-discover/GROUND_TRUTH.md plus
exact values computed directly from the raw corpus by the examiner
(/tmp/xerj-autoindex/gt_compute.py → /tmp/xerj-autoindex/ground_truth_computed.json).
Neither simulated agent ever sees this file, the manifest, or /tmp/xerj-discover/tools.

Fairness: every question is answerable from the raw files alone (grep/python/sqlite are all
available to the baseline) AND from the indexed data alone (no question references index names,
field names, or anything only autoindex would know). Both agents get the identical question text,
identical turn budget (40), identical model, one fresh invocation per question.

---

## Q1 — Inventory ("what is this data?")
**Question:** "Give me an inventory of this data: what kinds of data sources are present (logs, events, databases, documents, exports, ...) and roughly how many records/rows/lines does each major source contain?"
**Ground truth (correct = names most major families with order-of-magnitude counts):**
- events JSONL ~744,000 March records (+ ~80,000 Feb archive in .jsonl.gz)
- device metrics/telemetry JSONL ~120,000
- nginx access logs ~434,000 lines; app logs ~279,000 lines
- SQL dump: tenants 8, users 500, orders 120,000, audit_log 80,000, chat_usage 8,016
- SQLite: devices 300, sensor_readings 60,000
- CSV exports: orders 50,000; users 500; German semicolon sensor CSV 40,000
- docs: 600 HTML, 400 txt notes, 400 JSON, ~35 PDF/DOCX, YAML configs, XML
Scoring: correct = ≥6 families named with roughly right counts (±20% or right order of magnitude); partial = families right but counts largely missing/wrong; wrong = misses most of the picture.

## Q2 — Cross-source entity lookup
**Question:** "User u-1042 keeps coming up. In which distinct data sources (kinds of files/tables) does user u-1042 appear? List each source."
**Ground truth:** ≥5 distinct families: (1) events JSONL, (2) app logs (`user=u-1042`), (3) nginx logs (remote-user field), (4) SQL dump — users, orders, audit_log, chat_usage tables, (5) users.csv export. Also appears in the March-14 postmortem HTML. (u-1042 is a planted "hot user".)
Scoring: correct = ≥5 families incl. both a log family and the SQL/DB family; partial = 3–4; wrong = ≤2.

## Q3 — The March-14 incident story
**Question:** "Something went wrong on the morning of 2026-03-14. Reconstruct what happened: which tenant was affected, what time window, and which related identifiers (IP address, trace/request ID, incident URL) tie the story together across sources?"
**Ground truth:** search-service outage 2026-03-14 ~09:00–11:00Z affecting tenant **t-acme**; error spike in events (status=error cluster, first 25 records of events-10.jsonl), source IP **203.0.113.42**, trace UUID **7f3d9a2e-5b1c-4e8f-9a6d-2c4b8e1f7a30**, incident URL **https://status.meridianlabs.example/incidents/INC-2417**; nginx 503s + app-log ERROR lines; narrative postmortem docs/html/postmortems/2026-03-14-search-outage.html (mentions u-1042).
Scoring: correct = tenant + window + ≥2 of the 3 identifiers; partial = tenant + window, or ≥2 identifiers without tenant; wrong = neither.

## Q4 — Quantitative aggregation: revenue per tenant
**Question:** "Across ALL orders in this data, what is the total order amount (amount_usd) per tenant, and which tenant generated the most revenue? Give per-tenant totals."
**Ground truth (from full 120,000-row orders table in the SQL dump; the 50,000-row CSV is a subset):**
t-northwind $2,576,038.83 (top); t-umbrella $2,545,167.14; t-stark $2,474,356.62; t-acme $2,459,395.16; t-globex $2,375,772.85; t-initech $365,333.10; t-hooli $286,890.65; t-wonka $177,467.09. Total orders 120,000.
Scoring: correct = top tenant t-northwind + per-tenant totals within ~1% over all 120k orders (all 8 tenants or the 5 core with SQL-only ones at least present); partial = right ranking/top tenant but only the 50k CSV subset, or totals off; wrong = wrong top tenant.

## Q5 — Device cardinality + cross-source agreement
**Question:** "How many distinct devices exist in this data, and do the different sources that track devices agree with each other on the device inventory?"
**Ground truth:** exactly **300** distinct device ids; three sources: metrics/telemetry JSONL (`device_id`), SQLite analytics DB (devices + sensor_readings), German semicolon CSV (`geraet` column). Each has exactly 300, 100% set overlap.
Scoring: correct = 300 + at least 2 of the 3 sources checked and stated to agree; partial = 300 from one source only, or right sources but wrong count; wrong = other.

## Q6 — Status distribution of the event stream
**Question:** "In the event stream data, what status values occur and what is their distribution (counts or percentages)?"
**Ground truth (March events, 744,000):** ok 684,343 (92.0%), error 29,910 (4.02%), timeout 18,613 (2.5%), throttled 11,134 (1.5%). (With Feb archive incl. combined: ok 757,947 / error 33,062 / timeout 20,623 / throttled 12,368 / null 297 of 824,297 — also accepted.)
Scoring: correct = all 4 values named with ~right proportions (ok ≈ 92%); partial = values right but proportions absent/wrong; wrong = misses values.

## Q7 — Format-hostile fact (SQLite)
**Question:** "What device models are deployed in the fleet and how many devices of each model are there? Also, which tenant owns the most devices?"
**Ground truth (only in SQLite db/analytics.sqlite `devices`):** models PX-9 78, TH-100 77, PX-11 74, TH-200 71 (sum 300). Most devices: **t-globex** with 75 (then t-northwind 68, t-acme 55, t-stark 52, t-umbrella 50).
Scoring: correct = all 4 models with exact-ish counts + t-globex; partial = models or tenant only; wrong = neither.

## Q8 — Format-hostile fact (semicolon CSV, decimal-comma numbers)
**Question:** "What is the maximum temperature ever recorded in the March sensor export, and which device recorded it?"
**Ground truth (exports/csv/sensoren_export_maerz.csv, `temperatur_c` uses decimal COMMA):** max **98.6 °C** ("98,6"), device **dev-903fba**, 2026-03-22T18:02:57Z, Pune. (40,000 rows; avg 58.04.)
Scoring: correct = 98.6 + dev-903fba; partial = 98.6 without device (or device without value); wrong = other value (e.g. decimal-comma parsed wrong → 986 or 9.86 or a lexicographic max like "99,9"-style errors).
Note: SQLite sensor_readings.temp_c is a DIFFERENT sensor table; its max is not 98.6. The question says "March sensor export" — the CSV. If an agent answers from sqlite instead, score partial at best.

## Q9 — Negative-space cross-source join
**Question:** "Which tenants appear ONLY in the SQL database backup and never in any of the live telemetry (events, metrics, logs)?"
**Ground truth:** **t-initech, t-hooli, t-wonka** (SQL-only); the 5 core tenants (t-acme, t-globex, t-northwind, t-stark, t-umbrella) appear everywhere.
Scoring: correct = exactly those 3; partial = subset/superset with ≤1 mistake; wrong = other.

## Q10 — Junk / undecodable data
**Question:** "Is there anything in this data that is junk, corrupted, or not decodable? What is the file assets/textures/sprite-atlas.bin?"
**Ground truth:** sprite-atlas.bin = 4 MiB of random binary bytes — junk/undecodable, no indexable content. Other junk: docs/drafts/untitled.txt (0 bytes), notes/legacy/1998-readme-sjis.txt (Shift-JIS non-UTF8), notes/legacy/resume-latin1.txt (Latin-1 non-UTF8), events/archive/events-2026-02-27-part0.jsonl.gz (truncated gzip), exports/summary_q1.pdf (NOT a PDF — actually JSON, "quarter":"2026-Q1").
Scoring: correct = identifies sprite-atlas.bin as undecodable binary junk AND names ≥2 other junk/anomalous files; partial = sprite-atlas.bin only; wrong = claims it has real content.
