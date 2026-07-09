#!/usr/bin/env node
// gen-corpus.mjs — Northwind Robotics internal-KB corpus + ground-truth query set.
//
// Produces a wide, recursive folder of MIXED-format documents under ./corpus/
// (.pdf .docx .html .md .txt) plus ./queries.json, for the doc-index use case
// (see SPEC.md). The .pdf/.docx are REAL binaries: we author a flat-ODF (.fodt)
// source and convert it with LibreOffice (soffice), so the same extraction the
// indexer uses (pdftotext / soffice->txt) round-trips the answer text.
//
// The script is deterministic + idempotent: it wipes and regenerates corpus/
// every run, and it SELF-VERIFIES the ground-truth invariants that make the
// XERJ-vs-grep comparison honest (see verify() below). If any invariant is
// violated it throws — the corpus is never left in a subtly-wrong state.
//
// Requirements: node >= 18, soffice/libreoffice, pdftotext (poppler).
//
// Usage:  node gen-corpus.mjs
//
// Honesty note: this is synthetic but coherent prose for a fictional company
// ("Northwind Robotics"). No real personal names; roles only.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const CORPUS = path.join(HERE, "corpus");
const QUERIES_JSON = path.join(HERE, "queries.json");
// Scratch build area (soffice profile + intermediate .fodt / conversion output).
const BUILD = fs.mkdtempSync(path.join(os.tmpdir(), "nwr-corpus-"));
const PROFILE = path.join(BUILD, "profile");
const SOFFICE_ENV = `-env:UserInstallation=file://${PROFILE}`;

// ---------------------------------------------------------------------------
// Text-format corpus files (.html .md .txt). Written verbatim.
// `body` is the exact file content. Keep answer lines that BOTH approaches must
// find on a SINGLE physical line (baseline greps raw lines).
// ---------------------------------------------------------------------------
const TEXT_FILES = [
  // ---- HR -----------------------------------------------------------------
  {
    path: "hr/code-of-conduct.md",
    body: `# Northwind Robotics — Code of Conduct

We build autonomous fulfillment robots, and we hold ourselves to the same
standard of reliability we design into our machines. Every employee is expected
to act with honesty, respect, and good judgement.

## Respect in the workplace

Harassment, discrimination, and retaliation of any kind are prohibited. Treat
teammates, customers, and partners with courtesy regardless of role or seniority.

## Conflicts of interest

Disclose any outside activity or financial interest that could conflict with your
responsibilities at Northwind. When in doubt, raise it with People Operations.

## Reporting concerns

Concerns can be raised confidentially through the ethics line. Northwind does not
tolerate retaliation against anyone who reports a concern in good faith.
`,
  },
  {
    path: "hr/benefits/health-plan.md",
    body: `# Health and Wellbeing Benefits

Northwind Robotics offers a comprehensive benefits package to all full-time
team members, effective on the first day of employment.

## Medical, dental, and vision

The company pays 90% of the medical premium for employees and 70% for
dependents. Dental and vision coverage are included at no additional cost.

## Retirement

Northwind matches 401(k) contributions dollar-for-dollar up to 5% of salary.
Matching contributions vest immediately.

## Wellbeing stipend

Every team member receives an annual wellbeing stipend of $600, usable for gym
memberships, ergonomic equipment, or mental-health services.
`,
  },
  // ---- Engineering --------------------------------------------------------
  {
    path: "engineering/onboarding.md",
    body: `# Engineering Onboarding Guide

Welcome to the Northwind engineering organization. This guide walks incoming
engineers through their first two weeks.

## Day one — equipment

Every incoming software hire is issued a 16-inch developer workstation on their first day.
It ships pre-imaged with the standard toolchain, the VPN client, and access to
the internal package registry. If anything is missing, open a ticket with IT.

## First week

New hires pair with an onboarding buddy, complete the security-awareness module,
and ship a small documentation fix as their first pull request. By the end of the
first week you should have local builds running and be able to deploy to staging.

## Getting help

The engineering handbook lives in the internal wiki. For anything urgent, ask in
the team channel — someone is always around.
`,
  },
  {
    path: "engineering/architecture/system-overview.md",
    body: `# Platform Architecture Overview

The Northwind platform coordinates fleets of warehouse robots and the software
services that plan, dispatch, and track their work. This note summarizes the
major services and how state flows between them.

## Services

- **Ingest service** — receives fulfillment events from the warehouse floor.
  The ingest service listens on port 8412 for inbound NDJSON batches.
- **Planner** — turns orders into pick routes and assigns them to robots.
- **Inventory service** — the authoritative record of stock levels and locations.

## Persistence

We favor boring, well-understood storage. The inventory service stores its SKU records in a PostgreSQL 15 cluster.
A read replica serves reporting queries so that analytics never compete with the
live picking path. Event history is kept in an append-only object-store bucket.

## State flow

Floor events land in the ingest service, are normalized, and are fanned out to the
planner and the inventory service. The planner never writes stock levels directly;
it asks the inventory service, which is the single source of truth.
`,
  },
  {
    path: "engineering/architecture/data-pipeline.md",
    body: `# Data Pipeline Notes

The analytics pipeline turns raw floor events into the dashboards operations
teams rely on. It is intentionally simple and replayable.

## Ingestion limits

To protect the live path, inbound traffic is throttled per client.
The default API rate limit is 1000 requests per minute per client key.
Clients that exceed the limit receive a 429 and should back off exponentially.

## Stages

1. **Collect** — floor events are appended to the event log.
2. **Normalize** — events are validated and enriched with warehouse metadata.
3. **Aggregate** — five-minute rollups feed the operations dashboards.

## Replay

Because every stage reads from the append-only log, the pipeline can be replayed
from any offset to rebuild derived tables after a schema change.
`,
  },
  {
    path: "engineering/postmortems/incident-4471.txt",
    body: `INCIDENT 4471 — Delayed pick routes in Fulfillment Center 3
Status: Resolved

Summary
-------
For roughly 40 minutes, the planner issued pick routes several seconds late,
slowing throughput in one fulfillment center. No orders were lost.

Root cause
----------
A slow reporting query saturated the reporting replica and, through a shared
connection pool, starved the planner of connections. The planner fell back to
serial planning, which is correct but slow.

Resolution
----------
The offending query was moved to a dedicated read pool. Connection pools for the
live picking path are now isolated from reporting workloads.

Follow-ups
----------
- Add an alert for reporting-replica connection saturation.
- Document pool isolation in the architecture overview.
`,
  },
  // ---- Product ------------------------------------------------------------
  {
    path: "product/faq.html",
    body: `<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Northwind Robotics — Product FAQ</title></head>
<body>
<h1>Northwind Robotics — Product FAQ</h1>
<p>Answers to the questions we hear most often from operations teams evaluating
the Northwind fulfillment platform.</p>

<dl>
<dt>What uptime does the platform guarantee?</dt>
<dd>Our platform SLA guarantees 99.9% uptime measured monthly, excluding scheduled maintenance windows.</dd>

<dt>How quickly can we deploy?</dt>
<dd>A standard rollout takes two to four weeks, including floor mapping and a
supervised pilot before full production traffic.</dd>

<dt>What if a customer is unhappy with a unit?</dt>
<dd>Customers may send back any unit within 30 days of delivery for a full reimbursement, no questions asked.</dd>

<dt>Can the robots operate alongside people?</dt>
<dd>Yes. The fleet is designed for mixed human-and-robot floors, with safety
zones and speed limits enforced in software.</dd>
</dl>
</body>
</html>
`,
  },
  {
    path: "product/release-notes.md",
    body: `# Release Notes — Fulfillment Platform

## 2044.2 (current)

- Planner now batches short pick routes, improving throughput on dense shelves.
- Added a live heatmap of floor congestion to the operations dashboard.
- Reduced cold-start time for newly powered-on robots.

## 2044.1

- Introduced per-client throttling on the ingest path.
- Reporting queries moved to a dedicated read replica.
- Fixed a rare mis-sort when two robots contended for the same aisle.

## 2043.4

- Initial support for multi-zone warehouses.
- Faster firmware updates over the maintenance network.
`,
  },
  {
    path: "product/roadmap.txt",
    body: `NORTHWIND ROBOTICS — PRODUCT ROADMAP (INTERNAL)

Now
---
- Ship the congestion heatmap to all production customers.
- Harden the planner against connection-pool starvation (see incident 4471).

Next (Q3)
---------
- Q3 target: reach 99.5% pick accuracy across all warehouse fulfillment centers.
- Pilot autonomous charging hand-off between robots.
- Cut planner tail latency by half on dense shelves.

Later
-----
- Multi-site fleet balancing.
- Predictive maintenance from vibration telemetry.
`,
  },
  // ---- Security -----------------------------------------------------------
  {
    path: "security/access-control.html",
    body: `<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Access Control Guide</title></head>
<body>
<h1>Access Control Guide</h1>
<p>How access to Northwind systems is granted, reviewed, and recovered.</p>

<h2>Single sign-on</h2>
<p>All internal tools sit behind single sign-on. Access is granted by role, and
group membership is reviewed every quarter.</p>

<h2>Locked out?</h2>
<p>To recover access to a locked account, use the credential self-service portal and follow the emailed recovery link.</p>
<p>If the self-service flow does not resolve it, contact the IT service desk and
they will verify your identity before restoring access.</p>

<h2>Privileged access</h2>
<p>Administrative access to production is time-boxed and requires a second
approver. Every elevation is logged and expires automatically.</p>
</body>
</html>
`,
  },
  // ---- Finance ------------------------------------------------------------
  {
    path: "finance/procurement.html",
    body: `<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Procurement Guide</title></head>
<body>
<h1>Procurement Guide</h1>
<p>How to buy things at Northwind without slowing your team down.</p>

<h2>Approval thresholds</h2>
<p>Purchases under $500 can be made by any manager. Purchases over $5,000 must be approved by the Director of Operations before a purchase order is issued.</p>

<h2>Preferred vendors</h2>
<p>Use a preferred vendor where one exists; the terms are already negotiated.
New vendors must complete a security and financial review first.</p>

<h2>Receiving</h2>
<p>Match every delivery against its purchase order and flag discrepancies to
finance within five business days.</p>
</body>
</html>
`,
  },
  // ---- Operations ---------------------------------------------------------
  {
    path: "operations/facilities.md",
    body: `# Facilities and Ways of Working

Northwind's people are split across two hubs and a growing set of customer
fulfillment sites. This note covers how and where we work.

## Where we work

Northwind runs a distributed-first culture, and staff may work from home up to three days a week.
Teams come into the office on the remaining days for planning, workshops, and
hands-on time with hardware. Field engineers are on site at customer warehouses
as their assignments require.

## Office access

Badges are issued on your first day. Visitors must be signed in at the front desk
and escorted while on the floor near active robots.

## Facilities requests

Anything from a broken chair to a lab-space booking goes through the facilities
queue, which is triaged every morning.
`,
  },
  {
    path: "operations/vendor-directory.txt",
    body: `NORTHWIND ROBOTICS — APPROVED VENDOR DIRECTORY (INTERNAL)

Category            Vendor                 Notes
------------------  ---------------------  ----------------------------------
Cloud hosting       Meridian Cloud         Primary region us-central
Motors and drives   Kesten Drive Systems   Long-lead items, order early
Batteries           Voltline Cells         Preferred for all new robots
Logistics           Harbor Freightways     Pallet and crate shipping
Office supplies      Deskworks              Standard catalog only

Notes
-----
- Always confirm current pricing in the finance portal before raising an order.
- Report quality issues to procurement so the scorecard stays accurate.
`,
  },
  // ---- Meetings -----------------------------------------------------------
  {
    path: "meetings/2044-q1-planning.txt",
    body: `Q1 2044 PLANNING — NOTES
Attendees: Engineering, Product, Operations leads

Themes
------
- Reliability first: no throughput feature ships without a rollback plan.
- Close out the connection-pool isolation work from incident 4471.
- Prepare fulfillment centers for the Q3 pick-accuracy push.

Decisions
---------
- Freeze non-critical schema changes during peak season.
- Stand up a dedicated reporting replica in every region.
- Operations to publish a weekly congestion report.

Open questions
--------------
- Do we need a second on-call rotation for the analytics pipeline?
- What is the staffing plan for the new fulfillment site?
`,
  },
  {
    path: "meetings/decision-log.md",
    body: `# Engineering Decision Log

A running log of decisions with enough context to understand them later.

## Cadence

The weekly engineering sync is held every Wednesday at 10:00 in the main conference room.
Decisions made outside the sync are recorded here within one business day.

## Recent decisions

- **D-118** Isolate live-path connection pools from reporting. Rationale:
  incident 4471. Owner: platform team.
- **D-121** Adopt an append-only event log as the pipeline source of truth so
  derived tables can be rebuilt by replay.
- **D-124** Standardize on a single-sign-on group per service to make quarterly
  access reviews tractable.
`,
  },
  // ---- Legal --------------------------------------------------------------
  {
    path: "legal/ip-policy.html",
    body: `<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Intellectual Property Policy</title></head>
<body>
<h1>Intellectual Property Policy</h1>
<p>How Northwind treats the ideas and work created by its people.</p>

<h2>Ownership</h2>
<p>Work created in the course of employment, including designs, firmware, and
documentation, is owned by Northwind Robotics.</p>

<h2>Open source</h2>
<p>Contributing to open source is encouraged. Get sign-off from engineering
leadership before publishing anything derived from internal code.</p>

<h2>Prior inventions</h2>
<p>List any inventions you made before joining on your onboarding form so they are
clearly excluded from company ownership.</p>
</body>
</html>
`,
  },
  // ---- Support ------------------------------------------------------------
  {
    path: "support/troubleshooting.md",
    body: `# Field Troubleshooting Guide

Quick checks for the most common issues field engineers see on the warehouse
floor. Escalate to platform on-call if these do not resolve the problem.

## Robot will not start a route

1. Confirm the robot has network on the maintenance VLAN.
2. Check that the planner shows the robot as available.
3. Power-cycle the robot if firmware is mid-update.

## Slow picking in one aisle

Usually aisle congestion. Check the congestion heatmap; if a single robot is
stuck, clear the obstruction and it will re-plan automatically.

## Dashboards look stale

The analytics pipeline is replayable; a stale dashboard is almost always the
rollup stage catching up, not lost data.
`,
  },
  {
    path: "support/known-issues.txt",
    body: `NORTHWIND ROBOTICS — KNOWN ISSUES (INTERNAL)

- KI-207: Congestion heatmap can lag by up to a minute on very large floors.
  Workaround: refresh the dashboard. Fix planned for 2044.3.

- KI-212: Firmware update over a weak maintenance signal may retry several times.
  Workaround: move the robot closer to an access point before updating.

- KI-219: Reporting queries during peak can still queue briefly even with the
  dedicated replica. Being tracked under the pool-isolation work.
`,
  },
];

// ---------------------------------------------------------------------------
// Binary-format corpus files (.pdf .docx). Authored as flat-ODF blocks and
// converted with soffice. Answer sentences are kept SHORT (one printed line)
// so pdftotext -layout / soffice->txt round-trip them without line-wrap breaks.
// `blocks`: array of { h1 | h2 | p : string }.
// ---------------------------------------------------------------------------
const BINARIES = [
  {
    path: "hr/handbook.pdf",
    format: "pdf",
    blocks: [
      { h1: "Northwind Robotics — Employee Handbook" },
      { p: "This handbook summarizes the policies that apply to all Northwind Robotics employees. It complements, and does not replace, your individual offer letter." },
      { h2: "Paid time off" },
      { p: "Full-time staff accrue 22 days of paid time off each year." },
      { p: "Paid time off begins accruing on your first day and may be carried over up to ten days into the following calendar year. Requests are approved by your manager through the people portal." },
      { h2: "Parental leave" },
      { p: "Paid parental leave is 16 weeks for eligible full-time staff." },
      { p: "Parental leave is available to all full-time employees after one year of continuous service and may be taken following the birth or adoption of a child." },
      { h2: "Working hours" },
      { p: "Standard working hours are 40 per week. Northwind supports flexible scheduling arrangements subject to manager approval and team coverage." },
      { h2: "Standards of conduct" },
      { p: "Employees are expected to act with integrity, protect confidential information, and treat colleagues and customers with respect at all times." },
    ],
  },
  {
    path: "hr/benefits/leave-policy.docx",
    format: "docx",
    blocks: [
      { h1: "Leave and Absence Policy" },
      { p: "This policy describes the categories of leave available to Northwind Robotics employees beyond standard paid time off." },
      { h2: "Bereavement leave" },
      { p: "Bereavement leave grants up to 5 working days per event." },
      { p: "Bereavement leave applies to the loss of an immediate family member. Additional unpaid time can be arranged with People Operations on a case-by-case basis." },
      { h2: "Sick leave" },
      { p: "The company provides ten days of paid sick leave each year. Unused sick leave does not carry over but is refreshed at the start of each calendar year." },
      { h2: "Jury duty" },
      { p: "Employees called for jury service are granted paid time for the duration of their civic obligation, with no impact on their paid-time-off balance." },
    ],
  },
  {
    path: "engineering/oncall-runbook.docx",
    format: "docx",
    blocks: [
      { h1: "Platform On-Call Runbook" },
      { p: "This runbook is the first thing an on-call engineer should open when paged. It defines severity levels, response expectations, and common remediations." },
      { h2: "Severity levels" },
      { p: "Sev-1 is a full customer-facing outage. Sev-2 is degraded service. Sev-3 is a minor issue with no customer impact." },
      { p: "On-call must acknowledge a Sev-1 within 5 minutes of the page." },
      { h2: "Escalation" },
      { p: "Unacknowledged pages escalate to secondary on-call after 15 minutes." },
      { p: "If the secondary also does not acknowledge, the alert escalates to the engineering manager. Keep the incident channel updated at every step." },
      { h2: "Common remediations" },
      { p: "For a saturated reporting replica, shed reporting load and confirm the live picking path has a dedicated connection pool. For ingest backpressure, scale the ingest workers and verify clients are honoring throttling." },
    ],
  },
  {
    path: "security/data-classification.pdf",
    format: "pdf",
    blocks: [
      { h1: "Data Classification and Retention Standard" },
      { p: "This standard defines how Northwind Robotics classifies information and how long each category must be kept." },
      { h2: "Classification tiers" },
      { p: "Data is classified as Public, Internal, Confidential, or Restricted. The tier determines handling, access, and encryption requirements." },
      { h2: "Retention" },
      { p: "Customer transaction records are retained for 7 years." },
      { p: "Retention satisfies financial-audit and regulatory obligations, after which records are securely destroyed. Retention periods for other categories are listed in the appendix." },
      { h2: "Encryption" },
      { p: "All Restricted data must be encrypted at rest and in transit. Encryption keys are rotated on a regular schedule and stored in the managed key service." },
    ],
  },
  {
    path: "finance/expense-policy.pdf",
    format: "pdf",
    blocks: [
      { h1: "Travel and Expense Policy" },
      { p: "This policy explains what Northwind Robotics reimburses when you travel for work and how to submit an expense report." },
      { h2: "Meal per diem" },
      { p: "Domestic travel meals are reimbursed at $75 per day." },
      { p: "International per-diem rates vary by destination and are published in the finance portal. Alcohol is not reimbursable under the per-diem allowance." },
      { h2: "Airfare and lodging" },
      { p: "Economy class is the default for flights under six hours. Book lodging within the published nightly cap for the destination city." },
      { h2: "Submitting expenses" },
      { p: "Submit expense reports within 30 days of travel, with itemized receipts for anything above the minimum threshold. Reimbursement is paid with the next payroll cycle." },
    ],
  },
  // ---- Binary distractors (no query targets them, but XERJ still indexes them) ----
  {
    path: "product/datasheet.pdf",
    format: "pdf",
    blocks: [
      { h1: "Northwind R-Series Robot — Datasheet" },
      { p: "The R-Series is Northwind's flagship warehouse fulfillment robot, designed for mixed human-and-robot floors." },
      { h2: "Specifications" },
      { p: "Payload capacity is up to 40 kilograms. Top safe speed on the warehouse floor is limited in software for shared aisles." },
      { p: "A full charge supports a typical eight-hour shift, with opportunistic top-up charging between routes." },
      { h2: "Safety" },
      { p: "The R-Series enforces speed and separation limits near people, with redundant sensors and an independent emergency stop." },
    ],
  },
  {
    path: "legal/contractor-terms.docx",
    format: "docx",
    blocks: [
      { h1: "Independent Contractor Terms" },
      { p: "These standard terms govern engagements between Northwind Robotics and independent contractors." },
      { h2: "Confidentiality" },
      { p: "Contractors must keep all non-public Northwind information confidential during and after the engagement." },
      { h2: "Ownership of work" },
      { p: "All deliverables created under the engagement are assigned to Northwind Robotics upon creation." },
      { h2: "Term and termination" },
      { p: "Either party may end the engagement with two weeks written notice. Confidentiality obligations survive termination." },
    ],
  },
];

// ===========================================================================
// LARGE DOCUMENTS (>=60 KB each) — the context-efficiency demonstrators.
//
// The files above are deliberately small, which makes the "load the whole file
// vs. one ranked passage" context argument INVERT (a passage costs more than a
// 1 KB file). To demonstrate the context-efficiency win HONESTLY we need
// realistically large documents. Each of the four below buries ONE unique,
// plain-greppable answer line deep inside a >=60 KB body, so a FAIR grep
// baseline CAN find the line but must open the whole large file to use it,
// whereas XERJ returns a single ranked ~800-char passage.
//
//   - hr/employee-handbook.html            (HTML)  — referral-bonus figure
//   - engineering/api-reference.md         (MD)    — a config default deep in a
//                                                    big API/config reference
//   - engineering/runbooks/major-incident-runbook.txt (TXT) — a comms deadline
//   - security/information-security-policy.pdf (PDF) — a remediation SLA. Being
//     a PDF it is ALSO invisible to grep, so this one demonstrates format
//     coverage AND scale at once.
//
// All four share ONE content model — an array of blocks ({h1|h2|h3|p|li|code}) —
// rendered to HTML / MD / TXT, or to a PDF via the existing FODT->soffice path.
// Prose is coherent Northwind-Robotics content built from compact real specs and
// realistic policy/reference scaffolding — verbose like the real thing, not
// lorem ipsum, no personal names.
// ===========================================================================
const MIN_LARGE_BYTES = 60 * 1024; // 61,440 — the ">=60 KB" floor (SPEC + self-verify).

// ---- block -> format renderers -------------------------------------------
function htmlEscape(s) {
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
function renderHtml(title, blocks) {
  const out = ["<!doctype html>", '<html lang="en">',
    `<head><meta charset="utf-8"><title>${htmlEscape(title)}</title></head>`, "<body>"];
  let inList = false;
  const closeList = () => { if (inList) { out.push("</ul>"); inList = false; } };
  for (const b of blocks) {
    if (b.li == null) closeList();
    if (b.h1 != null) out.push(`<h1>${htmlEscape(b.h1)}</h1>`);
    else if (b.h2 != null) out.push(`<h2>${htmlEscape(b.h2)}</h2>`);
    else if (b.h3 != null) out.push(`<h3>${htmlEscape(b.h3)}</h3>`);
    else if (b.p != null) out.push(`<p>${htmlEscape(b.p)}</p>`);
    else if (b.code != null) out.push(`<pre><code>${htmlEscape(b.code)}</code></pre>`);
    else if (b.li != null) { if (!inList) { out.push("<ul>"); inList = true; } out.push(`<li>${htmlEscape(b.li)}</li>`); }
  }
  closeList();
  out.push("</body>", "</html>");
  return out.join("\n") + "\n";
}
function renderMd(blocks) {
  const out = [];
  for (const b of blocks) {
    if (b.h1 != null) out.push("# " + b.h1, "");
    else if (b.h2 != null) out.push("## " + b.h2, "");
    else if (b.h3 != null) out.push("### " + b.h3, "");
    else if (b.p != null) out.push(b.p, "");
    else if (b.code != null) out.push("```json", b.code, "```", "");
    else if (b.li != null) out.push("- " + b.li);
  }
  // keep list items tight, but a blank line after a list group
  return out.join("\n").replace(/\n{3,}/g, "\n\n").trim() + "\n";
}
function renderTxt(blocks) {
  const out = [];
  for (const b of blocks) {
    if (b.h1 != null) out.push(b.h1.toUpperCase(), "=".repeat(Math.min(b.h1.length, 72)), "");
    else if (b.h2 != null) out.push("", b.h2, "-".repeat(Math.min(b.h2.length, 72)), "");
    else if (b.h3 != null) out.push("", b.h3, "");
    else if (b.p != null) out.push(b.p, ""); // one paragraph == one physical line (keeps answers greppable)
    else if (b.code != null) out.push(b.code.split("\n").map((l) => "    " + l).join("\n"), "");
    else if (b.li != null) out.push("  - " + b.li);
  }
  return out.join("\n").replace(/\n{3,}/g, "\n\n").trim() + "\n";
}

// ---- 1) Employee handbook (HTML) -----------------------------------------
// Compact specs: [h2 title, purpose sentence, [key points]]. A scaffold expands
// each into a realistic multi-paragraph handbook section. One deep section
// carries the unique referral-bonus figure.
const HANDBOOK_POLICIES = [
  ["Purpose of this handbook", "This handbook summarizes the policies, benefits, and expectations that apply to everyone who works at Northwind Robotics.", ["It complements, and does not replace, your individual offer letter or any policy published on the internal wiki.", "Where a specific policy document conflicts with this summary, the specific policy governs.", "People Operations maintains this handbook and updates it as policies change."]],
  ["Our values", "Northwind builds autonomous fulfillment robots, and we hold ourselves to the reliability we design into our machines.", ["We value honesty, sound judgement, and respect for the people we work with and for.", "We prefer boring, well-understood solutions and clear communication over cleverness.", "We take ownership of our commitments and raise problems early."]],
  ["Equal opportunity", "Northwind is an equal-opportunity employer and does not tolerate discrimination or harassment.", ["Employment decisions are based on merit, qualifications, and business need.", "Harassment, discrimination, and retaliation of any kind are prohibited and will be investigated.", "Reasonable accommodations are available through People Operations on request."]],
  ["Employment basics", "Most roles at Northwind are full-time and at-will, subject to the terms of your offer letter.", ["Your first ninety days are an onboarding and mutual-fit period with regular check-ins.", "Changes to your role, level, or compensation are confirmed in writing.", "Employment records are kept confidential and handled under our data-protection standards."]],
  ["Working hours and flexibility", "Northwind supports flexible scheduling so long as team coverage and commitments are met.", ["Core collaboration hours are agreed within each team so meetings land when people are available.", "Flexible arrangements are approved by your manager and revisited if business needs change.", "Non-exempt staff must record hours accurately, including any overtime."]],
  ["Remote and hybrid work", "Northwind runs a distributed-first culture, and most teams work in a hybrid pattern.", ["Where a role allows it, staff may work remotely for part of the week by agreement with their manager.", "Teams come together on shared days for planning, workshops, and hands-on time with hardware.", "Field engineers work on site at customer warehouses as their assignments require."]],
  ["Attendance and time recording", "We rely on each other to be available and predictable during agreed hours.", ["Notify your manager and team as early as possible if you will be unexpectedly unavailable.", "Recurring absence is handled supportively through People Operations, not punitively.", "Accurate time and leave records keep payroll and coverage correct."]],
  ["Compensation and pay", "Compensation reflects the role, level, market data, and individual performance.", ["Salaries are paid on a regular cycle published in the payroll portal.", "Pay is reviewed annually and may be adjusted following a promotion or a significant change in scope.", "Questions about your pay are confidential and go to People Operations."]],
  ["Performance and growth", "We want everyone to know how they are doing and what growth looks like.", ["Managers hold regular one-to-ones and a lightweight periodic performance conversation.", "Growth is measured against a published set of role expectations for each level.", "Feedback is expected to be specific, timely, and kind."]],
  ["Promotions", "Promotions recognize sustained performance at the next level, not tenure alone.", ["A promotion case is written up by your manager and reviewed by a calibration panel.", "Calibration exists to keep the bar consistent across teams.", "If a case is not yet ready, you will get concrete, actionable feedback."]],
  ["Learning and development", "Northwind invests in the growth of its people through time and budget for learning.", ["Each employee has an annual professional-development budget for courses, books, and conferences.", "Time for approved learning is treated as work time, not personal time.", "Sharing what you learn back with your team is strongly encouraged."]],
  ["Referral program", "Great people know great people, and referrals are one of our best sources of hires.", ["Anyone may refer a candidate through the referral portal, subject to the usual fair-hiring rules.", "Referrers are kept informed of their candidate's progress at a high level.", "The program excludes hiring-manager and People-Operations referrals for roles they own."]],
  ["Code of conduct", "Every employee is expected to act with integrity and to treat others with respect.", ["Protect confidential and customer information at all times.", "Disclose conflicts of interest to People Operations when in doubt.", "Concerns can be raised confidentially through the ethics line without fear of retaliation."]],
  ["Health, safety, and the floor", "Safety around robots and hardware is a shared, non-negotiable responsibility.", ["Follow posted safety procedures in labs and on customer warehouse floors at all times.", "Visitors must be signed in and escorted while near active robots.", "Report near-misses and hazards immediately so we can fix them before harm occurs."]],
  ["Wellbeing", "We want Northwind to be a sustainable place to do your best work.", ["An annual wellbeing stipend can be used for fitness, ergonomics, or mental-health services.", "Confidential support is available through the employee assistance program.", "Managers are expected to model healthy working patterns, including real time off."]],
  ["Benefits overview", "Full-time team members are eligible for a comprehensive benefits package from day one.", ["Details of medical, dental, vision, and retirement benefits are in the benefits guide.", "Open enrollment happens once a year, with mid-year changes allowed for qualifying life events.", "The People Operations team can walk you through your options."]],
  ["Time off and leave", "Northwind offers paid time off plus several categories of protected leave.", ["Paid time off, holidays, and sick leave are described in the leave and absence policy.", "Family and medical leave are available to eligible employees under that policy.", "Plan longer absences with your manager so the team can arrange coverage."]],
  ["Equipment and IT", "Northwind issues the equipment you need and expects you to look after it.", ["Standard-issue hardware ships pre-imaged with the approved toolchain and security agent.", "Report lost or stolen devices to IT immediately so access can be revoked.", "Company equipment is for company work; incidental personal use is fine within the acceptable-use policy."]],
  ["Information security for everyone", "Security is part of every role, not just the security team's job.", ["Use single sign-on and multi-factor authentication on every internal system.", "Never share credentials, and store secrets only in the approved secret manager.", "Report anything suspicious, including phishing, to the security team right away."]],
  ["Travel and expenses", "When you travel for work, Northwind reimburses reasonable, documented costs.", ["Book within the published caps and use preferred vendors where they exist.", "Submit expenses promptly with itemized receipts through the finance portal.", "The travel and expense policy has the full rules, including per-diem details."]],
  ["Communications and social media", "Speak about Northwind with the same honesty we expect internally.", ["Do not share confidential or customer information in public forums.", "Make clear when you are speaking personally rather than for the company.", "Route press and analyst inquiries to the communications team."]],
  ["Grievances and disputes", "Everyone deserves a fair, confidential way to raise a workplace concern.", ["Raise concerns with your manager, People Operations, or the ethics line, whichever feels right.", "Concerns are handled promptly, confidentially, and without retaliation.", "Serious matters are escalated to an impartial reviewer."]],
  ["Leaving Northwind", "When someone moves on, we want the transition to be clean and respectful.", ["Give the notice stated in your offer letter and help hand over your work.", "Return company equipment and confirm your access has been revoked.", "Confidentiality obligations continue after your employment ends."]],
  ["Diversity and inclusion", "We want Northwind to be a place where people of every background can do their best work.", ["Hiring and promotion decisions are made on merit and are reviewed for fairness.", "Employee community groups are supported and open to all.", "Inclusive language and behaviour are expected of everyone, in every setting."]],
  ["Anti-harassment", "Harassment has no place at Northwind and is taken seriously whenever it is reported.", ["Harassment of any kind, in person or online, is prohibited.", "Reports are investigated promptly and confidentially.", "Retaliation against anyone who reports in good faith is itself a serious violation."]],
  ["Data protection", "We handle personal data lawfully, carefully, and only for legitimate purposes.", ["Access personal data only where you need it for your work.", "Store and share personal data using approved, secured tools.", "Report any suspected data loss to the security team immediately."]],
  ["Acceptable use of systems", "Company systems and equipment are provided for company work and must be used responsibly.", ["Keep incidental personal use reasonable and lawful.", "Do not install unapproved software on company equipment.", "Do not use company systems to store or share unlawful or inappropriate content."]],
  ["Expenses and reimbursement", "Northwind reimburses reasonable, documented business expenses promptly.", ["Spend company money as if it were your own.", "Keep itemized receipts and submit them through the finance portal.", "When a rule is unclear, ask finance before you commit the spend."]],
  ["Working with contractors", "Contractors are valued members of the team and are engaged under clear terms.", ["Contractor access is scoped to what the engagement needs.", "Confidentiality obligations apply to contractors as they do to employees.", "Deliverables created under an engagement belong to Northwind."]],
  ["Intellectual property", "Ideas and work created at Northwind are protected and, generally, owned by the company.", ["Work created in the course of employment is owned by Northwind.", "Get sign-off before publishing anything derived from internal work.", "List prior inventions on your onboarding form so they are clearly excluded."]],
  ["Health and safety responsibilities", "Everyone shares responsibility for a safe workplace, on hardware floors especially.", ["Follow posted safety procedures and never bypass a safety control.", "Report hazards and near-misses so they can be fixed.", "Complete required safety training before working near active robots."]],
  ["Environmental responsibility", "We try to run Northwind in a way that is considerate of its environmental impact.", ["Reduce waste and recycle where facilities allow.", "Prefer efficient options for travel and shipping.", "Suggest improvements through the facilities or sustainability channels."]],
  ["Onboarding and first days", "Your first days are designed to help you settle in and become productive without pressure.", ["You are paired with an onboarding buddy and given a clear first task.", "Equipment and access are arranged before you start where possible.", "Regular check-ins during onboarding make sure you have what you need."]],
  ["Feedback culture", "We give and receive feedback often, so nobody is surprised at review time.", ["Feedback is specific, timely, and kind.", "It flows in every direction, not just from managers.", "Acting on feedback matters more than merely collecting it."]],
  ["Meeting hygiene", "Meetings are a tool, not a default; we try to use them well and respect people's time.", ["Every meeting has a purpose and, where useful, an agenda.", "Decisions and actions are recorded so absentees stay informed.", "Prefer asynchronous updates when a meeting is not needed."]],
  ["Acknowledgement", "By working at Northwind you agree to follow the policies summarized here.", ["Ask People Operations if anything in this handbook is unclear.", "Policies are reviewed regularly and you will be told when material changes are made.", "The current version of every policy always lives on the internal wiki."]],
];
const HANDBOOK_GLOSSARY = [
  ["Accrual", "the gradual build-up of a leave balance over the year as it is earned."],
  ["At-will employment", "an arrangement where either party may end the employment relationship, subject to your offer terms and applicable law."],
  ["Calibration", "a cross-team review that keeps performance and promotion decisions consistent."],
  ["Confidential information", "any non-public information about Northwind, its people, its customers, or its technology."],
  ["Conflict of interest", "a situation where a personal interest could improperly influence your work decisions."],
  ["Core hours", "the agreed window when a team expects members to be available for collaboration."],
  ["Dependent", "a family member eligible for coverage under Northwind's benefit plans."],
  ["Employee assistance program", "a confidential service offering counselling and practical support."],
  ["Ethics line", "a confidential channel for raising concerns about conduct or compliance."],
  ["Exempt employee", "a salaried role not eligible for overtime under applicable law."],
  ["Full-time", "a role scheduled for the standard full working week and eligible for the full benefits package."],
  ["Hybrid work", "a pattern that mixes on-site and remote days by agreement with your manager."],
  ["Immediate family", "the close relatives defined in the leave and absence policy for bereavement and family leave."],
  ["Level", "a role's position on the career framework, with published expectations."],
  ["Non-exempt employee", "an hourly role eligible for overtime, required to record hours accurately."],
  ["Onboarding period", "the initial mutual-fit period after joining, with extra check-ins and support."],
  ["Open enrollment", "the annual window to choose or change your benefit elections."],
  ["People Operations", "the team that owns hiring, benefits, policy, and employee support."],
  ["Per diem", "a fixed daily allowance for meals and incidentals while travelling for work."],
  ["Preferred vendor", "a supplier with pre-negotiated terms that should be used where one exists."],
  ["Probationary check-in", "a structured conversation during the onboarding period about fit and progress."],
  ["Qualifying life event", "a change such as marriage or a new child that allows mid-year benefit changes."],
  ["Reasonable accommodation", "an adjustment that enables an employee to perform their role."],
  ["Retaliation", "any adverse action taken against someone for raising a concern in good faith; strictly prohibited."],
  ["Single sign-on", "one authenticated identity used to access internal tools, protected by multi-factor authentication."],
  ["Stipend", "a fixed allowance provided for a specific purpose, such as wellbeing."],
  ["Vesting", "the point at which a benefit, such as an employer retirement match, becomes fully yours."],
  ["Wellbeing stipend", "an annual allowance for fitness, ergonomics, or mental-health services."],
  ["Acceptable use", "the rules governing appropriate use of company systems and equipment."],
  ["Time in lieu", "time off granted to balance approved additional hours worked."],
  ["Benefits portal", "the online system where you review and enrol in your benefits and find in-network providers."],
  ["People portal", "the system used to request time off, view leave balances, and update personal details."],
  ["Finance portal", "the system used to submit expenses, claim stipends, and manage travel bookings."],
  ["Career framework", "the published set of role expectations for each level that guides growth and promotion."],
  ["One-to-one", "a regular private conversation between an employee and their manager."],
  ["Hub", "one of Northwind's main offices, as distinct from a customer fulfillment site."],
  ["Field engineer", "an engineer who works on site at customer warehouses as assignments require."],
  ["Offer letter", "the document setting out your individual terms of employment, which governs where it is more specific than this handbook."],
  ["Notice period", "the amount of advance notice you or the company must give to end employment, stated in your offer letter."],
  ["Open source contribution", "work published to a public project, which requires leadership sign-off when derived from internal code."],
  ["Prior invention", "an invention made before joining Northwind, which you list on onboarding so it is excluded from company ownership."],
  ["Escort policy", "the requirement that visitors are signed in and accompanied while near active robots."],
  ["Ethics line", "a confidential channel for raising conduct or compliance concerns without fear of retaliation."],
  ["Income protection", "insurance that replaces part of your income during extended illness or disability."],
];
const HANDBOOK_FAQ = [
  ["When do benefits start?", "Eligible full-time team members are covered from their first day of employment."],
  ["How do I request time off?", "Submit the request to your manager through the people portal; approval keeps the team's coverage clear."],
  ["Can I carry time off into next year?", "A limited carry-over is allowed; the exact amount is set out in the leave and absence policy."],
  ["How often is pay reviewed?", "Pay is reviewed once a year and may also change after a promotion or a significant change in scope."],
  ["What is the professional-development budget for?", "Approved courses, books, certifications, and conferences that help you grow in your role."],
  ["Who do I talk to about a benefits question?", "People Operations can walk you through medical, dental, vision, and retirement options."],
  ["What should I do if I lose my laptop?", "Report it to IT immediately so device access can be revoked and a replacement issued."],
  ["How do I raise a concern about conduct?", "Speak to your manager or People Operations, or use the confidential ethics line."],
  ["Is retaliation really prohibited?", "Yes. Retaliation against anyone who raises a concern in good faith is strictly prohibited."],
  ["Can I work fully remotely?", "It depends on the role; where it is possible, arrange it with your manager and keep team coverage in mind."],
  ["Do I record my hours?", "Non-exempt employees must record hours accurately, including any overtime; exempt roles do not."],
  ["What happens during my first ninety days?", "A structured onboarding and mutual-fit period with regular check-ins from your manager."],
  ["How do promotions work?", "Your manager writes a case that a calibration panel reviews to keep the bar consistent across teams."],
  ["Can I contribute to open source?", "Yes, with sign-off from engineering leadership before publishing anything derived from internal code."],
  ["What is the wellbeing stipend?", "An annual allowance you can spend on fitness, ergonomic equipment, or mental-health services."],
  ["How do I book work travel?", "Book within the published caps, prefer negotiated vendors, and keep itemized receipts for expenses."],
  ["When are expenses reimbursed?", "Submit promptly through the finance portal; approved expenses are paid with the next payroll cycle."],
  ["Who approves flexible schedules?", "Your manager, provided team coverage and commitments are maintained."],
  ["What if I have a qualifying life event?", "You can change your benefit elections mid-year; contact People Operations to make the change."],
  ["How do I refer someone?", "Submit them through the referral portal; you will be kept informed of their progress at a high level."],
  ["Are references to policies binding?", "The full policy documents on the wiki govern; this handbook is a plain-language summary."],
  ["What should I do about a phishing email?", "Report it to the security team right away and do not click links or enter credentials."],
  ["Can I use my work laptop for personal things?", "Incidental personal use is fine within the acceptable-use policy; keep it reasonable and lawful."],
  ["Who owns work I create at Northwind?", "Work created in the course of employment is owned by Northwind, as set out in the IP policy."],
  ["What notice do I give if I resign?", "Give the notice stated in your offer letter and help hand over your responsibilities cleanly."],
  ["Where is the latest version of a policy?", "Always on the internal wiki; if the handbook and a policy differ, the policy wins."],
  ["Do visitors need to be escorted?", "Yes. Visitors are signed in at the front desk and escorted while near active robots."],
  ["How is my personal data handled?", "Under Northwind's data-protection standards, confidentially and only for legitimate purposes."],
  ["Can I get an ergonomic chair?", "Facilities requests, including ergonomic equipment, go through the facilities queue or the wellbeing stipend."],
  ["What if I disagree with a performance rating?", "Discuss it with your manager first; unresolved concerns can go to People Operations for an impartial review."],
  ["How much notice do I need to book time off?", "There is no fixed minimum, but book as early as you reasonably can so your team can arrange coverage around your absence."],
  ["Can I take time off during my first ninety days?", "Yes, subject to the same approval as any other time; just plan it with your manager as part of settling in."],
  ["What happens to unused sick leave at year end?", "Sick leave does not carry over; it is refreshed at the start of each calendar year rather than accumulated."],
  ["Do I get paid for public holidays I work?", "If your role requires cover on a public holiday, time in lieu is arranged so you are not out of pocket."],
  ["How do I change my benefit elections?", "During open enrollment, or mid-year if you have a qualifying life event; make the change through the benefits portal or People Operations."],
  ["Who counts as a dependent for benefits?", "Eligibility is defined in the plan documents; People Operations can confirm whether a specific family member qualifies."],
  ["Can I expense a home-office purchase?", "Use the home-office allowance for setup items and the wellbeing stipend for ergonomics; check the eligible-purchase list first."],
  ["What is the difference between a hub and a fulfillment site?", "A hub is a Northwind office; a fulfillment site is a customer warehouse where field engineers work on assignment."],
  ["How do I report a safety hazard?", "Report it immediately through the facilities queue or to a manager; near-misses should be reported too, not just injuries."],
  ["Can I bring a visitor into the office?", "Yes, but sign them in at the front desk and escort them at all times while near active robots."],
  ["What should I do before I travel for work?", "Book within the caps, use preferred vendors, and record your itinerary in the travel portal so travel insurance applies."],
  ["How do I claim the wellbeing stipend?", "Buy an eligible item or service and submit the claim through the finance portal; the eligible list is deliberately broad."],
  ["Is my use of the assistance program private?", "Yes; the employee assistance program is confidential and does not go through your manager."],
  ["Can I change my retirement contribution rate?", "Yes, at any time through the payroll portal; the company match applies up to the published percentage."],
  ["What if my equipment breaks?", "Open a ticket with IT; if it cannot be fixed quickly a replacement is arranged so you are not blocked."],
  ["How do I request an internal move?", "Talk to your manager and watch the internal postings; open roles are advertised internally first."],
  ["What is expected during my notice period?", "Give the notice in your offer letter, hand over your work cleanly, and return company equipment before you leave."],
  ["Can I keep my accounts after leaving?", "No; access is revoked on your last day, and confidentiality obligations continue afterwards."],
  ["How are pay reviews decided?", "They consider your role, level, market data, and performance, and are confirmed to you in writing."],
  ["What if I am asked to do something that feels wrong?", "Raise it with your manager, People Operations, or the ethics line; you will not face retaliation for a good-faith concern."],
  ["Do contractors follow the same policies?", "Contractors follow the security, confidentiality, and conduct policies; their engagement terms cover the specifics."],
  ["Can I speak at a conference about my work?", "Often yes, with sign-off from leadership so that nothing confidential is disclosed; the communications team can help."],
  ["How do I find the current holiday list?", "It is published annually on the internal wiki for each hub's location."],
  ["What if I need to work adjusted hours for a while?", "Talk to your manager; flexible and adjusted arrangements are supported as long as team coverage is maintained."],
  ["How do I nominate someone for recognition?", "Use the recognition program on the wiki; nominations are quick and genuinely appreciated."],
  ["Can I volunteer during work hours?", "Yes; a set amount of paid volunteering time is available each year, arranged with your manager."],
  ["What ergonomic support can I get at home?", "Request a remote ergonomic assessment through facilities and use the home-office allowance for recommended items."],
  ["Who owns a side project I build in my own time?", "If it is unrelated to Northwind's business and uses no company resources it is generally yours; check the IP policy and disclose if unsure."],
  ["How do I escalate a facilities problem?", "Log it in the facilities queue, which is triaged every morning; flag anything urgent to facilities directly."],
  ["What if I cannot resolve a dispute with a colleague?", "Try a direct conversation first; if that does not work, your manager or People Operations can help mediate."],
  ["Can I work from another country temporarily?", "Sometimes, but it has tax and legal implications, so clear it with People Operations well in advance rather than assuming."],
  ["How do I get added to a community group?", "Community groups are employee-led and open; find them on the wiki and just reach out to join."],
  ["What counts as confidential information?", "Any non-public information about Northwind, its people, its customers, or its technology; when unsure, treat it as confidential."],
  ["Can I accept a gift from a vendor?", "Modest, occasional gifts may be fine, but disclose anything that could look like it influences a business decision."],
  ["How do I set up multi-factor authentication?", "IT provisions it during onboarding; enrol a strong second factor and keep a backup method registered."],
  ["What should I do if I receive a phishing email?", "Report it with one click and do not interact with it; the security team would rather see ten false alarms than miss one real attack."],
  ["Do I need approval to buy software for work?", "Yes; unapproved software must not be installed on company equipment, and IT can help you find an approved alternative."],
  ["How often are performance conversations held?", "Lightweight conversations happen regularly through one-to-ones, with a periodic more formal check-in against role expectations."],
  ["Can I mentor or be mentored?", "Yes to both; mentoring is encouraged and People Operations can help match you with someone."],
  ["What if my role changes significantly?", "A meaningful change in scope is confirmed in writing and may be reflected in your level and pay at the next review."],
  ["How do I book a meeting room or lab space?", "Through the facilities queue or the room-booking system; lab space near hardware may need extra safety sign-off."],
  ["Are lunch-and-learns recorded?", "Where the presenter is comfortable, yes, so distributed and future colleagues can catch up; check before assuming."],
  ["What is the process for a sabbatical?", "Apply ahead of time; approval depends on tenure, team coverage, and a clean handover of your responsibilities."],
  ["Can I donate my volunteering time to a team effort?", "Yes; teams sometimes pool volunteering time for a shared cause, arranged with your managers."],
  ["How do I update my emergency contact?", "Keep it current in the people portal so we can reach the right person if something happens at work."],
  ["What if I am injured at work?", "Get first aid, report it immediately, and let People Operations know; workplace injuries are taken seriously and followed up."],
  ["Can I use company equipment for a side project?", "Incidental use is fine within the acceptable-use policy, but company equipment and time should not power an unrelated venture."],
  ["How are decisions communicated to remote staff?", "Decisions are recorded in writing so that people who were not in the room, including remote and future colleagues, stay informed."],
  ["What happens if I lose my badge?", "Report it to facilities immediately so it can be deactivated and a replacement issued."],
  ["Where can I ask a question that is not covered here?", "Ask your manager or People Operations, or search the internal wiki, which holds the authoritative version of every policy."],
];
const HANDBOOK_BENEFITS = [
  ["Medical cover", "Northwind pays the large majority of the medical premium for employees and a substantial share for dependents, and coverage begins on your first day with no waiting period. The plan covers preventive care, specialist visits, hospital treatment, and prescriptions. You can review the plan documents and find in-network providers through the benefits portal, and People Operations can help you choose the option that fits your circumstances."],
  ["Dental cover", "Routine and major dental care are covered under the standard plan at no additional premium to the employee, and preventive visits are fully covered to encourage regular care. The plan includes checkups, cleanings, fillings, and a share of the cost of major work. Keeping up with routine visits is the cheapest way to avoid larger problems later."],
  ["Vision cover", "Eye examinations and an allowance toward lenses or frames are included each year, and staff in screen-heavy roles are reminded to use the benefit. The allowance can also be applied to prescription safety eyewear where a role requires it. Book through any in-network provider and claim any balance through the portal."],
  ["Retirement matching", "The company matches retirement-plan contributions up to a published percentage of salary, and the match vests immediately, so it is yours from day one. You choose your own contribution rate and can change it at any time through the payroll portal. Even a small regular contribution compounds meaningfully over a career, and the match is effectively part of your total compensation."],
  ["Life and disability", "Basic life cover and income protection are provided at no cost, giving you and your family a financial cushion in the worst cases. You can buy additional cover for yourself or dependents during open enrollment. Keep your beneficiary details current in the portal so cover pays out as you intend."],
  ["Employee assistance", "A confidential assistance program offers counselling and practical support for you and your household, available around the clock. It covers everything from stress and relationships to legal and financial questions. Using it is private, does not go through your manager, and is there precisely for the hard moments."],
  ["Wellbeing stipend", "An annual stipend can be spent on fitness, ergonomic equipment, or mental-health services, and is claimed through the finance portal. The list of eligible purchases is deliberately broad because wellbeing looks different for everyone. Spend it on what genuinely helps you stay healthy and do good work."],
  ["Learning budget", "Each employee has an annual budget for courses, books, certifications, and conferences that support growth in their role. Approved learning happens on work time, not your own, and sharing what you learn with your team is strongly encouraged. Talk to your manager about how to spend it in a way that advances both your goals and the team's."],
  ["Home-office allowance", "A one-time allowance helps remote and hybrid staff set up a safe, productive workspace at home. It can go toward a desk, chair, monitor, or other ergonomic essentials. If you are not sure what you need, request an ergonomic assessment first."],
  ["Commuter support", "Where offered locally, pre-tax commuter benefits or a transit allowance help offset the cost of getting to a hub. The exact scheme depends on your location and is described in the local benefits guide. Enrol through the portal before the deadline for the tax advantage to apply."],
  ["Parental support", "Beyond statutory leave, new parents get a phased return-to-work option and access to family-support resources. A gradual return helps you and your team adjust without a cliff edge. People Operations can walk you through leave, pay, and the practicalities well before your leave begins."],
  ["Fertility and family building", "The plan includes support toward fertility and family-building services, handled confidentially through the benefits provider. It recognizes that families are formed in many ways. Details of what is covered and how to access it are in the benefits portal, and the process is private."],
  ["Mental-health days", "A small number of dedicated mental-health days each year are separate from sick leave and need no explanation. They exist so that looking after your mental health does not have to compete with time off for illness. Use them proactively rather than waiting until you are running on empty."],
  ["Volunteering time", "Employees may take paid time each year to volunteer with a cause they care about, arranged with their manager. Teams sometimes volunteer together as a way to give back and to spend time together off the clock. Log volunteering time like any other planned absence so coverage is clear."],
  ["Referral bonus", "Employees who refer a candidate who is hired receive a referral bonus, subject to the fair-hiring rules in the referral policy. Referrals are one of our best sources of great colleagues. The full mechanics, including timing and eligibility, are described in the referral-program section above."],
  ["Recognition awards", "Peer and manager recognition programs celebrate outstanding work with small awards throughout the year. Recognition is most meaningful when it is specific and timely, so do not wait for a formal cycle to thank a colleague. Nominations are quick to submit and genuinely appreciated."],
  ["Sabbatical option", "Longer-tenured employees may apply for a sabbatical, subject to team coverage and manager approval. A sabbatical is a chance to rest deeply, learn something new, or pursue a personal project. Plan it well ahead so your team can prepare and so you can truly switch off."],
  ["Equipment refresh", "Standard-issue hardware is refreshed on a regular cycle so your tools stay reliable and safe. If your equipment is slowing you down before its refresh date, raise it with IT rather than struggling on. Returned equipment is securely wiped and responsibly recycled or redeployed."],
  ["Discounted services", "From time to time Northwind negotiates discounts on services that are useful to staff, and these are listed on the wiki. Availability varies by location and changes over time. Check the current list before assuming a discount applies."],
  ["Travel insurance", "Business travel is covered by company travel insurance, so keep your itinerary in the travel portal so cover applies. The policy covers medical emergencies, cancellations, and lost belongings while you are travelling for work. Familiarize yourself with the emergency contact details before you depart."],
];
const HANDBOOK_LEAVE = [
  ["Paid time off", "Full-time staff accrue paid time off through the year and request it through the people portal, and a limited balance can be carried into the next year. Book it early where you can so your team can plan around your absence. Time off is there to be used; managers are expected to model taking real breaks."],
  ["Public holidays", "Northwind observes the public holidays of each hub's location, and the current list is published annually on the wiki. Where a role requires cover on a public holiday, time in lieu is arranged. Distributed teams are mindful that holidays differ across locations when scheduling."],
  ["Sick leave", "Paid sick leave is available each year for your own illness or medical appointments and is refreshed at the start of the calendar year. Let your manager know as early as you can so the team can cover, and focus on getting better. For longer illness, medical leave and income protection take over."],
  ["Parental leave", "Eligible employees receive paid parental leave following the birth or adoption of a child, with the specifics set out in the leave and absence policy. A phased return-to-work option helps you ease back in. Start the conversation with People Operations well before your leave so everything is arranged in good time."],
  ["Family and caregiver leave", "Time to care for an immediate family member is available, and longer arrangements can be made with People Operations. Caring responsibilities are a normal part of life and are treated with understanding. Talk to your manager about the flexibility you need."],
  ["Bereavement leave", "Paid time is provided following the loss of an immediate family member, with additional unpaid time available on request. Grief does not follow a schedule, so the policy is applied compassionately. People Operations can help with the practicalities when you are ready."],
  ["Jury and civic duty", "Employees called for jury service or other civic duty are granted paid time for the duration of the obligation, with no impact on their paid-time-off balance. Simply share your summons with your manager and People Operations. Serving is a civic responsibility we support."],
  ["Medical and recovery leave", "Extended medical leave is available for serious health conditions and is coordinated with income-protection cover. The focus is on your recovery, not on paperwork. People Operations manages the process confidentially and keeps it as simple as possible."],
  ["Unpaid leave", "Where the paid categories do not fit, unpaid leave can be arranged with your manager and People Operations. It is useful for planned personal projects, extended travel, or circumstances that do not fall under another category. Agree the duration and return date in advance so your role can be covered."],
  ["Compassionate leave", "Short-notice compassionate leave is available for personal emergencies, so talk to your manager as early as you can. The aim is to remove work worries when something urgent happens in your life. Formalities can wait until the immediate situation is under control."],
  ["Study leave", "Time for approved study or examinations related to your role may be granted alongside the learning budget. It recognizes that some growth needs focused, uninterrupted time. Agree the plan with your manager so it dovetails with your other commitments."],
  ["Sabbatical leave", "A longer, planned absence for rest or a personal project is available to longer-tenured staff by application. A sabbatical works best when it is planned well ahead and your responsibilities are handed over cleanly. It is a chance to come back refreshed with a new perspective."],
];
const HANDBOOK_PERKS = [
  ["Flexible hours", "Within agreed core hours, shape your day around when you do your best work and when your team needs you. Some people are sharpest early, others late; flexibility lets both thrive. The only firm expectation is that you are reachable and reliable during the hours your team has agreed."],
  ["Hybrid working", "Split your week between a hub and home where your role allows, by agreement with your manager. In-person days are for the things that are simply better together, like workshops and hands-on hardware time. Remote days are protected for focused, heads-down work."],
  ["Quiet and focus spaces", "Hubs provide quiet areas for deep work alongside collaboration spaces for workshops and pairing. Use the space that fits the task rather than defaulting to your desk. Respect the quiet zones as genuinely quiet."],
  ["On-site refreshments", "Hubs stock tea, coffee, and healthy snacks, and kitchens are shared spaces, so please keep them tidy. Good coffee and a moment away from the screen are small things that add up. Clean up after yourself so the space stays pleasant for everyone."],
  ["Team socials", "Each team has a modest budget for regular get-togethers, in person or online for distributed teams. Socials work best when they are inclusive and not always centred on one activity. Rotate who plans them so everyone gets a say."],
  ["Hardware choice", "Within the standard, safe catalog you can choose the workstation configuration that suits your work. The catalog exists so equipment stays supportable and secure, not to limit you unnecessarily. If your work genuinely needs something outside it, make the case to IT."],
  ["Internal mobility", "Open roles are posted internally first, so talk to your manager if you are curious about a move. Growing by moving across the company is encouraged, not seen as disloyalty. A good manager will help you find your next step even when it is on another team."],
  ["Mentoring", "Formal and informal mentoring connects you with people who can help you grow. A mentor need not be senior to you in the org chart, only ahead of you in something you want to learn. People Operations can help you find a match."],
  ["Lunch-and-learns", "Regular informal sessions let people share what they are working on or learning. They are low-pressure and open to everyone, whatever your level. Volunteering to present is a great, gentle way to build confidence and share knowledge."],
  ["Ergonomic assessments", "Request an ergonomic assessment for your workstation, at a hub or at home, through facilities. A few small adjustments prevent a lot of discomfort over a career. Do not wait until something aches to ask."],
  ["Pet-friendly days", "Some hubs run pet-friendly days, so check local guidelines and be considerate of colleagues. Not everyone is comfortable around animals, and that is respected. Where they run, they are a cheerful addition to the week."],
  ["Community groups", "Employee community groups bring together people with shared interests and backgrounds. They are employee-led, open to allies, and supported by the company. They are a good way to meet people beyond your immediate team."],
];
function buildHandbookBlocks() {
  const b = [];
  const H1 = "Northwind Robotics — Employee Handbook";
  b.push({ h1: H1 });
  b.push({ p: "Welcome to Northwind Robotics. This handbook is your plain-language guide to how we work together, what you can expect from us, and what we ask of you. Please read it during onboarding and keep it handy — you will not need to memorize it, but you should know where to look." });
  b.push({ p: "It is organized as a series of short policy sections, followed by a glossary and a set of frequently asked questions. Every section points to the fuller policy on the internal wiki where more detail is needed." });
  const WHY_FRAMES = [
    (t) => `Why it matters. Getting "${t}" right protects both you and your colleagues and keeps Northwind a place people are glad to work.`,
    (t) => `Why it matters. Clear expectations around "${t}" prevent misunderstandings and let people focus on their work with confidence.`,
    (t) => `Why it matters. Handling "${t}" consistently is part of treating each other, and our customers, fairly.`,
    (t) => `Why it matters. When "${t}" is well understood, small issues are resolved early instead of becoming bigger problems.`,
  ];
  const WHERE_FRAMES = [
    "If anything here is unclear for your situation, check with your manager or People Operations before acting; the authoritative version lives on the internal wiki.",
    "When in doubt, ask first — People Operations would far rather answer a question than untangle a misunderstanding. The full policy is on the wiki.",
    "Your manager and People Operations are there to help you apply this sensibly. The complete, current policy is always on the internal wiki.",
    "Common sense and this summary usually agree; when they seem not to, raise it rather than guessing. The full policy is maintained on the wiki.",
  ];
  HANDBOOK_POLICIES.forEach(([title, purpose, points], i) => {
    b.push({ h2: title });
    b.push({ p: purpose });
    b.push({ p: "What this means in practice. This section sets the expectation; the points below spell out how it works day to day." });
    for (const pt of points) b.push({ li: pt.charAt(0).toUpperCase() + pt.slice(1) + "." });
    b.push({ p: WHY_FRAMES[i % WHY_FRAMES.length](title.toLowerCase()) });
    b.push({ p: WHERE_FRAMES[i % WHERE_FRAMES.length] });
    if (title === "Referral program") {
      // Buried unique answer line (single <p> => single physical line).
      b.push({ p: "Northwind pays an employee referral bonus of $3,000 per successful hire, paid after the referred candidate completes ninety days of employment." });
      b.push({ p: "The bonus applies to most non-executive roles, is split evenly if two employees refer the same candidate, and is subject to the fair-hiring rules described above. There is no limit on how many people you may refer in a year." });
    }
  });
  b.push({ h2: "Benefits in detail" });
  b.push({ p: "The full details of every benefit, including eligibility and how to enrol, are in the benefits guide on the wiki. This is a plain-language overview of what is on offer." });
  for (const [name, desc] of HANDBOOK_BENEFITS) { b.push({ h3: name }); b.push({ p: desc }); }
  b.push({ h2: "Leave types at a glance" });
  b.push({ p: "Northwind offers paid time off plus several categories of protected and supported leave. The leave and absence policy is the authoritative source for eligibility and durations; plan longer absences with your manager so the team can arrange coverage." });
  for (const [name, desc] of HANDBOOK_LEAVE) { b.push({ h3: name }); b.push({ p: desc }); }
  b.push({ h2: "Ways of working and perks" });
  b.push({ p: "Beyond formal benefits, a number of everyday things make Northwind a good place to do your best work." });
  for (const [name, desc] of HANDBOOK_PERKS) { b.push({ h3: name }); b.push({ p: desc }); }
  b.push({ h2: "Glossary" });
  b.push({ p: "A quick reference to terms used throughout this handbook and the underlying policies." });
  for (const [t, d] of HANDBOOK_GLOSSARY) b.push({ li: `${t}: ${d}` });
  b.push({ h2: "Frequently asked questions" });
  b.push({ p: "Answers to the questions People Operations hears most often. If yours is not here, just ask." });
  for (const [q, a] of HANDBOOK_FAQ) { b.push({ h3: q }); b.push({ p: a }); }
  return { path: "hr/employee-handbook.html", format: "html", title: H1, blocks: b };
}

// ---- 2) API & configuration reference (Markdown) -------------------------
// A big reference is naturally long and enumerated: many endpoints with request
// parameters, response fields, and JSON examples, plus a configuration section.
// The unique answer (a config default) is buried deep in the config reference.
const API_RESOURCES = [
  { s: "robot", p: "robots", base: "/v1/robots", about: "a physical fulfillment robot enrolled in a fleet", filter: "fleet_id, status, and model",
    fields: [["id", "string", "stable robot identifier"], ["model", "string", "hardware model, for example R-200"], ["fleet_id", "string", "fleet the robot belongs to"], ["status", "string", "one of idle, picking, charging, maintenance, or offline"], ["battery_pct", "integer", "current charge from 0 to 100"], ["firmware_version", "string", "running firmware build"], ["last_seen", "timestamp", "time of the last heartbeat"]],
    action: ["recall", "post", "Recall the robot to its charging dock and mark it unavailable for new routes."] },
  { s: "fleet", p: "fleets", base: "/v1/fleets", about: "a logical group of robots managed together in one warehouse", filter: "warehouse_id and status",
    fields: [["id", "string", "fleet identifier"], ["name", "string", "human-readable fleet name"], ["warehouse_id", "string", "warehouse the fleet operates in"], ["robot_count", "integer", "number of enrolled robots"], ["status", "string", "active or paused"]],
    action: ["pause", "post", "Pause dispatch to every robot in the fleet without unenrolling them."] },
  { s: "warehouse", p: "warehouses", base: "/v1/warehouses", about: "a physical fulfillment site", filter: "region",
    fields: [["id", "string", "warehouse identifier"], ["name", "string", "site name"], ["region", "string", "geographic region code"], ["zone_count", "integer", "number of mapped zones"], ["active", "boolean", "whether the site is live"]] },
  { s: "zone", p: "zones", base: "/v1/zones", about: "a mapped area within a warehouse", filter: "warehouse_id",
    fields: [["id", "string", "zone identifier"], ["warehouse_id", "string", "parent warehouse"], ["name", "string", "zone label"], ["kind", "string", "one of storage, staging, or charging"], ["congestion", "number", "recent congestion score from 0 to 1"]] },
  { s: "location", p: "locations", base: "/v1/locations", about: "a specific shelf or bin coordinate", filter: "zone_id and occupied",
    fields: [["id", "string", "location identifier"], ["zone_id", "string", "zone containing the location"], ["aisle", "string", "aisle label"], ["occupied", "boolean", "whether stock is present"], ["sku_id", "string", "SKU currently stored, if any"]] },
  { s: "sku", p: "skus", base: "/v1/skus", about: "a stock-keeping unit in the catalog", filter: "category",
    fields: [["id", "string", "SKU identifier"], ["title", "string", "product title"], ["category", "string", "catalog category"], ["weight_g", "integer", "unit weight in grams"], ["hazmat", "boolean", "whether special handling is required"]] },
  { s: "inventory record", p: "inventory", base: "/v1/inventory", about: "the authoritative stock level for a SKU at a location", filter: "sku_id and location_id",
    fields: [["id", "string", "record identifier"], ["sku_id", "string", "the SKU"], ["location_id", "string", "the storage location"], ["on_hand", "integer", "units physically present"], ["reserved", "integer", "units reserved for orders"]] },
  { s: "order", p: "orders", base: "/v1/orders", about: "a customer fulfillment order", filter: "status and warehouse_id",
    fields: [["id", "string", "order identifier"], ["warehouse_id", "string", "fulfilling warehouse"], ["status", "string", "one of received, planned, picking, packed, or shipped"], ["line_count", "integer", "number of order lines"], ["created_at", "timestamp", "when the order was received"]],
    action: ["cancel", "post", "Cancel an order that has not yet entered the picking stage."] },
  { s: "route", p: "routes", base: "/v1/routes", about: "a planned pick route assigned to a robot", filter: "robot_id and status",
    fields: [["id", "string", "route identifier"], ["robot_id", "string", "assigned robot"], ["order_id", "string", "order being fulfilled"], ["stop_count", "integer", "number of pick stops"], ["status", "string", "one of planned, active, or done"]] },
  { s: "pick", p: "picks", base: "/v1/picks", about: "a single pick action within a route", filter: "route_id",
    fields: [["id", "string", "pick identifier"], ["route_id", "string", "parent route"], ["location_id", "string", "location picked from"], ["sku_id", "string", "SKU picked"], ["quantity", "integer", "units picked"]] },
  { s: "task", p: "tasks", base: "/v1/tasks", about: "an asynchronous background job", filter: "kind and status",
    fields: [["id", "string", "task identifier"], ["kind", "string", "job type, for example reindex or export"], ["status", "string", "one of queued, running, done, or failed"], ["progress", "integer", "percent complete from 0 to 100"], ["created_at", "timestamp", "when the task was created"]] },
  { s: "alert", p: "alerts", base: "/v1/alerts", about: "an operational alert raised by the platform", filter: "severity and acknowledged",
    fields: [["id", "string", "alert identifier"], ["severity", "string", "one of info, warning, or critical"], ["source", "string", "service that raised the alert"], ["acknowledged", "boolean", "whether an operator has acknowledged it"], ["raised_at", "timestamp", "when the alert fired"]],
    action: ["acknowledge", "post", "Acknowledge the alert so it stops paging and is marked as owned."] },
  { s: "incident", p: "incidents", base: "/v1/incidents", about: "a tracked operational incident", filter: "severity and status",
    fields: [["id", "string", "incident identifier"], ["severity", "string", "one of sev-1, sev-2, or sev-3"], ["status", "string", "open, mitigated, or resolved"], ["summary", "string", "short description"], ["opened_at", "timestamp", "when the incident was declared"]] },
  { s: "firmware build", p: "firmware", base: "/v1/firmware", about: "a robot firmware image available for rollout", filter: "channel",
    fields: [["id", "string", "build identifier"], ["version", "string", "semantic version string"], ["channel", "string", "one of stable, beta, or canary"], ["size_bytes", "integer", "image size in bytes"], ["signed", "boolean", "whether the image signature is valid"]] },
  { s: "webhook", p: "webhooks", base: "/v1/webhooks", about: "a subscription that delivers events to your endpoint", filter: "event and active",
    fields: [["id", "string", "webhook identifier"], ["url", "string", "your HTTPS delivery endpoint"], ["event", "string", "event type to deliver"], ["active", "boolean", "whether delivery is enabled"], ["created_at", "timestamp", "when the subscription was created"]] },
  { s: "API key", p: "api-keys", base: "/v1/api-keys", about: "a credential used to authenticate API calls", filter: "active",
    fields: [["id", "string", "key identifier"], ["label", "string", "human-readable label"], ["scopes", "string", "space-separated permission scopes"], ["active", "boolean", "whether the key may be used"], ["created_at", "timestamp", "when the key was issued"]] },
  { s: "reservation", p: "reservations", base: "/v1/reservations", about: "a temporary hold placed on stock while an order is fulfilled", filter: "sku_id and status",
    fields: [["id", "string", "reservation identifier"], ["sku_id", "string", "the reserved SKU"], ["order_id", "string", "the order the hold is for"], ["quantity", "integer", "units held"], ["status", "string", "one of held, consumed, or expired"], ["expires_at", "timestamp", "when the hold lapses if unused"]] },
  { s: "shipment", p: "shipments", base: "/v1/shipments", about: "a packed order handed to a carrier", filter: "carrier and status",
    fields: [["id", "string", "shipment identifier"], ["order_id", "string", "the fulfilled order"], ["carrier", "string", "the carrier handling delivery"], ["tracking", "string", "carrier tracking reference"], ["status", "string", "one of packed, dispatched, or delivered"]] },
  { s: "audit event", p: "audit-events", base: "/v1/audit-events", about: "an immutable record of a change made through the API", filter: "actor and action",
    fields: [["id", "string", "event identifier"], ["actor", "string", "the API key or principal that acted"], ["action", "string", "the operation performed"], ["target", "string", "the resource affected"], ["at", "timestamp", "when the action occurred"]] },
  { s: "metric series", p: "metrics", base: "/v1/metrics", about: "a named time series of operational measurements", filter: "name and warehouse_id",
    fields: [["id", "string", "series identifier"], ["name", "string", "metric name, for example picks_per_minute"], ["warehouse_id", "string", "warehouse the series belongs to"], ["unit", "string", "unit of measurement"], ["last_value", "number", "most recent sampled value"]] },
];
const API_CONFIG = [
  ["ingest", [
    ["ingest.listen_port", "integer", "8412", "TCP port the ingest service binds for inbound NDJSON batches from the warehouse floor."],
    ["ingest.max_batch_size", "integer", "4096", "__BURY_BATCH__"],
    ["ingest.worker_threads", "integer", "8", "Number of worker threads that normalize and fan out inbound events."],
    ["ingest.queue_high_watermark", "integer", "50000", "Queue depth at which the service applies backpressure and returns 503 to new batches."],
    ["ingest.accept_timeout_ms", "integer", "2000", "How long a client connection may block waiting for the service to accept a batch."],
  ]],
  ["planner", [
    ["planner.max_stops_per_route", "integer", "24", "Upper bound on the number of pick stops the planner will place on one route."],
    ["planner.replan_interval_ms", "integer", "500", "How often the planner re-evaluates open routes against current floor state."],
    ["planner.connection_pool_size", "integer", "32", "Size of the dedicated database connection pool reserved for the live picking path."],
    ["planner.tail_latency_budget_ms", "integer", "150", "Target ceiling for planner tail latency before it sheds optional work."],
  ]],
  ["inventory", [
    ["inventory.db_url", "string", "postgres://inventory", "Connection string for the authoritative inventory PostgreSQL cluster."],
    ["inventory.read_replica_url", "string", "postgres://inventory-ro", "Connection string for the read replica that serves reporting queries."],
    ["inventory.reservation_ttl_s", "integer", "900", "How long a stock reservation is held before it expires if the order does not progress."],
    ["inventory.low_stock_threshold", "integer", "5", "On-hand level at or below which a low-stock alert is raised for a SKU."],
  ]],
  ["api", [
    ["api.listen_port", "integer", "9209", "Port the public REST API binds."],
    ["api.rate_limit_per_min", "integer", "1000", "Default per-client request ceiling per minute before requests are throttled with 429."],
    ["api.max_page_size", "integer", "200", "Largest page size a client may request on a list endpoint."],
    ["api.request_timeout_ms", "integer", "30000", "Server-side timeout applied to a single API request."],
    ["api.cors_allowed_origins", "string", "*", "Comma-separated list of origins permitted to call the API from a browser."],
  ]],
  ["storage", [
    ["storage.event_bucket", "string", "nwr-events", "Object-store bucket holding the append-only event log."],
    ["storage.retention_days", "integer", "365", "How long derived export artifacts are kept before lifecycle deletion."],
    ["storage.multipart_threshold_mb", "integer", "64", "Object size above which uploads switch to multipart."],
  ]],
  ["telemetry", [
    ["telemetry.sample_rate", "number", "0.1", "Fraction of traces sampled and exported to the observability backend."],
    ["telemetry.metrics_interval_s", "integer", "15", "How often service metrics are flushed to the metrics pipeline."],
    ["telemetry.log_level", "string", "info", "Default log level; one of debug, info, warn, or error."],
  ]],
  ["auth", [
    ["auth.token_ttl_s", "integer", "3600", "Lifetime of an issued access token in seconds."],
    ["auth.mfa_required", "boolean", "true", "Whether multi-factor authentication is required for interactive sign-in."],
    ["auth.session_idle_timeout_s", "integer", "1800", "Idle time after which an interactive session is invalidated."],
  ]],
];
function apiSampleValue(name, type) {
  if (type === "integer") return name.includes("pct") ? 87 : name.includes("port") ? 8412 : name.includes("count") ? 12 : 3;
  if (type === "boolean") return true;
  if (type === "number") return 0.42;
  if (type === "timestamp") return "2044-05-01T12:00:00Z";
  const strings = { id: "r-1a2b3c", model: "R-200", status: "idle", name: "north-fleet", version: "2044.2.0", severity: "warning", kind: "storage", channel: "stable", url: "https://example.com/hooks/nwr", label: "reporting-key", scopes: "read:robots read:orders" };
  return strings[name] || `${name.replace(/[^a-z]/gi, "")}-sample`;
}
function apiExample(r, includeAll) {
  const o = {};
  for (const [name, type] of r.fields) {
    if (!includeAll && (name === "id" || type === "timestamp")) continue;
    o[name] = apiSampleValue(name, type);
  }
  return JSON.stringify(o, null, 2);
}
function buildApiRefBlocks() {
  const b = [];
  const H1 = "Northwind Robotics Platform API — Reference";
  b.push({ h1: H1 });
  b.push({ p: "This is the complete reference for the Northwind Robotics Platform API. It documents every resource, its endpoints, the request parameters and response fields, and the configuration keys that govern each service. It is intended for integrators building on top of the fulfillment platform." });
  b.push({ p: "The API is a conventional JSON REST API. All requests and responses use UTF-8 JSON, all timestamps are RFC 3339 in UTC, and all identifiers are opaque strings that you should treat as case-sensitive and not parse." });
  b.push({ h2: "Base URL and versioning" });
  b.push({ p: "The API is served at https://api.northwind.example over TLS only. The version is embedded in the path, for example /v1/robots. Breaking changes are introduced under a new version prefix; additive changes may appear within a version without notice, so clients must ignore unknown fields." });
  b.push({ h2: "Authentication" });
  b.push({ p: "Every request must carry an API key in the Authorization header as a bearer token. Keys are scoped: a key may only call endpoints covered by its scopes, and a request outside a key's scopes returns 403. Rotate keys regularly and never embed them in client-side code." });
  b.push({ code: 'GET /v1/robots\nAuthorization: Bearer nwr_live_9f8e7d6c5b4a\nAccept: application/json' });
  b.push({ h2: "Request and response conventions" });
  b.push({ p: "List endpoints return a paginated envelope with a data array and a page object. Single-resource endpoints return the resource object directly. Write endpoints accept a JSON body and return the created or updated resource. Successful writes return 200 or 201; deletes return 204 with no body." });
  b.push({ h2: "Pagination" });
  b.push({ p: "List endpoints accept page and page_size query parameters. The default page size is 50 and the maximum is 200. The response page object reports the current page, the page size, and the total number of matching records so a client can iterate deterministically." });
  b.push({ h2: "Filtering and sorting" });
  b.push({ p: "Most list endpoints accept filter parameters named after indexed fields, and a sort parameter of the form field or -field for descending order. Filters combine with logical AND. Unknown filter parameters are rejected with 400 so typos fail loudly rather than silently returning everything." });
  b.push({ h2: "Rate limiting" });
  b.push({ p: "Requests are limited per API key per minute. When a client exceeds its ceiling the API returns 429 with a Retry-After header, and the client should back off exponentially. The default ceiling and other limits are listed in the configuration reference below." });
  b.push({ h2: "Idempotency" });
  b.push({ p: "Unsafe create requests may include an Idempotency-Key header. If the same key is replayed within twenty-four hours the API returns the original result instead of creating a duplicate, which makes retries after a network error safe." });
  b.push({ h2: "Errors" });
  b.push({ p: "Errors use standard HTTP status codes and a JSON body with a code and a human-readable message. The common codes are listed below." });
  for (const [code, meaning] of [["400 Bad Request", "the request was malformed or a parameter was invalid"], ["401 Unauthorized", "the API key was missing or invalid"], ["403 Forbidden", "the API key lacks the required scope"], ["404 Not Found", "no resource exists with the given identifier"], ["409 Conflict", "the write conflicts with the current state, for example a duplicate"], ["413 Payload Too Large", "the request body exceeded a configured size limit"], ["422 Unprocessable Entity", "the body was valid JSON but failed a business rule"], ["429 Too Many Requests", "the client exceeded its rate limit"], ["500 Internal Server Error", "an unexpected error; safe to retry with backoff"], ["503 Service Unavailable", "the service is applying backpressure; retry later"]]) {
    b.push({ li: `${code} — ${meaning}.` });
  }
  b.push({ h2: "Endpoints" });
  for (const r of API_RESOURCES) {
    b.push({ h3: `The ${r.p} resource` });
    b.push({ p: `A ${r.s} represents ${r.about}.` });
    const writable = r.fields.filter(([n, t]) => n !== "id" && t !== "timestamp");
    const fieldBullets = (fields) => { for (const [n, t, d] of fields) b.push({ li: `\`${n}\` (${t}) — ${d}.` }); };
    // list
    b.push({ h3: `GET ${r.base}` });
    b.push({ p: `List ${r.p} visible to the caller. Results are paginated and may be filtered by ${r.filter} and sorted by any indexed field. Use this endpoint to enumerate ${r.p} for dashboards and reconciliation jobs.` });
    b.push({ p: "Query parameters:" });
    b.push({ li: "`page` (integer, optional) — 1-based page number; defaults to 1." });
    b.push({ li: "`page_size` (integer, optional) — items per page, up to 200; defaults to 50." });
    b.push({ li: "`sort` (string, optional) — an indexed field, prefixed with `-` for descending order." });
    b.push({ p: `Each ${r.s} in the \`data\` array has the following fields:` });
    fieldBullets(r.fields);
    b.push({ p: "Example response:" });
    b.push({ code: `{\n  "data": [\n${apiExample(r, true).split("\n").map((l) => "    " + l).join("\n")}\n  ],\n  "page": { "page": 1, "page_size": 50, "total": 128 }\n}` });
    // create
    b.push({ h3: `POST ${r.base}` });
    b.push({ p: `Create a new ${r.s}. The request body carries the writable fields; server-managed fields such as \`id\` and timestamps are ignored if supplied and set by the server. Returns 201 with the created ${r.s}.` });
    b.push({ p: "Request body fields:" });
    fieldBullets(writable);
    b.push({ p: "Example request body:" });
    b.push({ code: apiExample(r, false) });
    // get
    b.push({ h3: `GET ${r.base}/{id}` });
    b.push({ p: `Fetch a single ${r.s} by its identifier. Returns the full ${r.s} representation, or 404 if no ${r.s} exists with that id. The response includes every field listed above.` });
    b.push({ li: "`id` (string, path) — the identifier of the ${r.s} to fetch." });
    // update
    b.push({ h3: `PATCH ${r.base}/{id}` });
    b.push({ p: `Partially update a ${r.s}. Only the fields present in the body are changed; omitted fields are left as they are. Returns the updated ${r.s}, or 409 if the update conflicts with the current state. Any of these writable fields may be supplied:` });
    fieldBullets(writable);
    // delete
    b.push({ h3: `DELETE ${r.base}/{id}` });
    b.push({ p: `Delete a ${r.s}. Returns 204 on success and is idempotent: deleting an already-deleted ${r.s} also returns 204. Deletion may be rejected with 409 if other records still reference it.` });
    // optional action
    if (r.action) {
      const [name, method, desc] = r.action;
      b.push({ h3: `${method.toUpperCase()} ${r.base}/{id}/${name}` });
      b.push({ p: `${desc} This action is asynchronous where it affects hardware and returns the updated ${r.s} with its new status. A 409 is returned if the ${r.s} is in a state where the action does not apply.` });
    }
  }
  b.push({ h2: "Configuration reference" });
  b.push({ p: "Each service reads its configuration from a namespaced key space. Keys may be set through the config file, an environment variable, or the admin API; the admin API takes precedence, then the environment, then the file. The tables below list every key, its type, its default, and what it controls. Defaults are chosen to be safe for a mid-sized single-warehouse deployment; tune them for your scale." });
  for (const [svc, keys] of API_CONFIG) {
    b.push({ h3: `${svc} configuration` });
    b.push({ p: `Keys in the ${svc} namespace control the ${svc} service. Restart the service after changing a file-based key; admin-API changes take effect immediately.` });
    for (const [key, type, def, desc] of keys) {
      if (desc === "__BURY_BATCH__") {
        // Buried unique answer line (single list item => single physical line).
        b.push({ li: `\`${key}\` (${type}, default \`${def}\`) — Maximum records accepted in a single bulk request; the ingest max_batch_size parameter defaults to 4096 records per bulk request, and larger batches are rejected with HTTP 413.` });
      } else {
        b.push({ li: `\`${key}\` (${type}, default \`${def}\`) — ${desc}` });
      }
    }
  }
  b.push({ h2: "Webhooks" });
  b.push({ p: "Subscribe to events by creating a webhook with an HTTPS endpoint and an event type. Northwind delivers each event as a signed JSON POST and retries with exponential backoff for up to twenty-four hours if your endpoint does not return 2xx. Verify the signature header on every delivery before trusting the payload." });
  b.push({ h2: "SDKs and changelog" });
  b.push({ p: "Official SDKs wrap this API for common languages and handle authentication, pagination, and retries for you. The changelog on the developer portal records every additive change within a version and every new version prefix, with migration notes." });
  return { path: "engineering/api-reference.md", format: "md", title: H1, blocks: b };
}

// ---- 3) Major-incident runbook (TXT) -------------------------------------
// A long operational runbook: severity model, roles, comms, a bank of service
// playbooks, an alert reference, and checklists. The unique answer (a comms
// deadline) is buried in the communications section.
const RUNBOOK_PLAYBOOKS = [
  ["Ingest backpressure / rising queue depth", "New floor events are accepted slowly or rejected, and dashboards fall behind real time.",
    ["Queue depth on the ingest service is climbing toward the high watermark.", "Clients are seeing 503 responses on batch submission.", "The event lag metric is rising steadily."],
    ["Check ingest.worker_threads utilization and CPU headroom.", "Confirm downstream (planner, inventory) are consuming, not stalled.", "Look for a single misbehaving client flooding batches."],
    ["Scale ingest workers horizontally to drain the queue.", "Throttle or block the offending client key at the edge.", "If a downstream is stalled, treat that as the primary incident and fail this one over to it."]],
  ["Planner connection-pool starvation", "Pick routes are issued late and throughput in one or more centers drops.",
    ["Planner tail latency exceeds its budget.", "Database connection-wait time on the live path is elevated.", "The planner logs show fallbacks to serial planning."],
    ["Check whether reporting queries are saturating a shared pool.", "Confirm the live path is using its dedicated pool, not the reporting pool.", "Inspect for a long-running or runaway query on the replica."],
    ["Shed reporting load and move heavy queries to the dedicated read pool.", "Confirm pool isolation between the live path and reporting.", "If a runaway query is found, terminate it and capture it for follow-up."]],
  ["Inventory divergence", "Reported stock levels disagree with physical counts or reservations do not release.",
    ["Reservations exceed on-hand for one or more SKUs.", "The reconciliation job reports a growing delta.", "Operators report picks failing at supposedly-stocked locations."],
    ["Check the reservation TTL and whether expiries are being processed.", "Confirm the event log replayed cleanly after any recent schema change.", "Look for a stuck normalize or aggregate stage."],
    ["Restart the stalled pipeline stage and let it catch up from the log.", "Manually expire stale reservations for affected SKUs after confirming with operations.", "Trigger a targeted reconciliation for the affected locations."]],
  ["API elevated error rate", "External integrators see 5xx responses or high latency from the public API.",
    ["The API 5xx rate is above its alert threshold.", "Latency p99 on the API is elevated.", "One dependency (auth, inventory) is slow or erroring."],
    ["Identify whether errors are concentrated on one endpoint or dependency.", "Check the rate-limiter for false positives after a config change.", "Confirm auth token issuance is healthy."],
    ["If one dependency is at fault, mitigate that dependency and shed load from the API.", "Roll back a recent API deploy if the error rate tracks it.", "Communicate a degraded-service status to affected integrators."]],
  ["Robot fleet offline in one zone", "A cluster of robots stops responding and picking halts in part of a warehouse.",
    ["Multiple robots in one zone show as offline within a short window.", "The maintenance VLAN in that zone looks unhealthy.", "No corresponding software deploy occurred."],
    ["Check the zone's network access points and switch health.", "Confirm it is not a firmware rollout gone wrong.", "Verify power to the charging infrastructure in that zone."],
    ["Engage facilities and network on the physical layer as the likely cause.", "Pause the affected fleet to stop dispatching into a dead zone.", "Fail routes over to an adjacent healthy zone if layout allows."]],
  ["Bad firmware rollout", "Robots that took a new firmware build misbehave or drop offline after an update.",
    ["Failures correlate with a specific firmware version.", "Robots on the previous build are healthy.", "The rollout channel recently advanced."],
    ["Confirm the version boundary between healthy and unhealthy robots.", "Check that the build signature was valid before rollout.", "Review the firmware changelog for the suspect build."],
    ["Halt the rollout immediately on all channels.", "Roll affected robots back to the last known-good build.", "Quarantine the bad build so it cannot be re-selected."]],
  ["Reporting replica saturation", "Analytics and dashboards slow down and may impact the live path if pools are shared.",
    ["Replica CPU or IO is pegged.", "Reporting query latency is high.", "Live-path latency rises if isolation is imperfect."],
    ["Identify the heaviest reporting queries.", "Confirm the live path is isolated from reporting connections.", "Check for a missing index after a schema change."],
    ["Move or kill the heaviest queries and add the missing index.", "Confirm and, if needed, harden pool isolation.", "Rate-limit ad-hoc reporting during peak."]],
  ["Object-store / event-log unavailable", "The append-only event log cannot be read or written, stalling the pipeline.",
    ["Writes to the event bucket fail or time out.", "The pipeline cannot advance past collect.", "Object-store health checks are red."],
    ["Confirm whether it is a provider outage or a credential/permission problem.", "Check bucket-level throttling or quota.", "Verify network path to the object store."],
    ["If provider-side, follow the vendor's status and buffer events locally if possible.", "Rotate or fix credentials if it is an auth failure.", "Once restored, let the pipeline replay from the last committed offset."]],
  ["Authentication / SSO outage", "Users and services cannot obtain tokens, blocking sign-in and internal calls.",
    ["Token issuance fails or is very slow.", "Interactive sign-in fails across tools.", "Service-to-service calls return 401."],
    ["Check the identity provider's health and recent changes.", "Confirm clock skew is within tolerance for token validation.", "Look for an expired signing certificate."],
    ["If a certificate expired, rotate it and redeploy.", "Engage the identity provider if it is provider-side.", "Communicate the outage and expected recovery to all staff."]],
  ["Deploy-induced regression", "Service health degrades immediately after a deploy to production.",
    ["Error rate or latency steps up at the deploy timestamp.", "The previous version was healthy.", "The change is in the suspect service's path."],
    ["Correlate the metric change precisely with the deploy time.", "Check the deploy's diff for the likely culprit.", "Confirm no coincident infrastructure change."],
    ["Roll back to the previous version as the first action.", "Confirm health recovers after rollback before investigating further.", "Open a follow-up to fix forward safely."]],
  ["Certificate expiry", "TLS connections fail because a certificate has expired or is about to.",
    ["Clients report certificate-validation errors.", "A certificate's not-after date has passed or is imminent.", "The affected endpoint served fine until the expiry moment."],
    ["Identify the exact certificate and where it is terminated.", "Confirm the renewal automation did or did not run.", "Check for multiple endpoints sharing the certificate."],
    ["Issue and deploy a renewed certificate immediately.", "Fix the renewal automation so it does not recur.", "Audit for other certificates nearing expiry."]],
  ["Disk / storage exhaustion", "A service degrades or crashes because a volume has filled up.",
    ["A volume is at or near 100 percent used.", "Writes fail or the service restarts in a loop.", "Log or spool growth is the likely cause."],
    ["Identify what is consuming the space.", "Check whether log rotation or cleanup stopped.", "Confirm it is not a runaway export or dump."],
    ["Free space by rotating or shipping logs and removing safe temporary files.", "Grow the volume if the growth is legitimate.", "Restore the cleanup job and add a headroom alert."]],
  ["Database primary failover", "The inventory primary becomes unavailable and writes stall until a replica is promoted.",
    ["Writes to the inventory service fail or time out.", "The primary is unreachable or in a crash loop.", "Replication lag on standbys is being watched closely."],
    ["Confirm the primary is genuinely down, not just slow.", "Check which standby is most caught up.", "Verify the failover automation is armed, not paused."],
    ["Let the automation promote the healthiest standby, or promote manually if it is paused.", "Repoint the inventory service at the new primary.", "Confirm write path health, then rebuild a fresh standby."]],
  ["Message backlog on the event bus", "Downstream consumers fall behind and derived data goes stale.",
    ["Consumer lag is growing across one or more topics.", "Dashboards and rollups are behind real time.", "No consumer errors, just slowness."],
    ["Identify the slowest consumer group.", "Check whether a single partition is hot.", "Confirm consumers are not blocked on a downstream call."],
    ["Scale the lagging consumer group out.", "Rebalance partitions if one is hot.", "If a downstream is the bottleneck, treat that as the primary incident."]],
  ["Cache stampede", "A cache expiry or flush causes a surge of expensive origin queries.",
    ["Origin query load spikes sharply.", "Latency rises across cache-backed endpoints.", "Cache hit rate drops suddenly."],
    ["Confirm whether a mass expiry or a flush just occurred.", "Check for a thundering-herd pattern on one key.", "Verify the origin is not itself degraded."],
    ["Enable request coalescing so only one origin fetch runs per key.", "Warm the hottest keys before re-enabling traffic.", "Stagger future expiries to avoid synchronized misses."]],
  ["Time synchronization drift", "Clock skew across hosts breaks token validation, ordering, or scheduling.",
    ["Token validation fails intermittently.", "Event ordering looks wrong.", "Hosts report differing times."],
    ["Check the time-sync service on the affected hosts.", "Measure the actual skew against a trusted source.", "Confirm it is not a single bad host."],
    ["Restart or fix time synchronization on drifting hosts.", "Isolate any host that cannot be corrected.", "Re-validate token flows once clocks agree."]],
  ["Third-party dependency outage", "An external provider Northwind relies on is degraded or down.",
    ["Calls to the provider fail or time out.", "The provider's status page reports an issue.", "Only features using that provider are affected."],
    ["Confirm it is provider-side, not our integration.", "Check whether a cached or degraded mode is available.", "Estimate blast radius across features."],
    ["Switch affected features to degraded mode where one exists.", "Communicate the limited impact to customers.", "Follow the provider's status and retry once restored."]],
  ["Runaway background job", "A background job consumes excessive resources and starves foreground work.",
    ["CPU, memory, or IO is dominated by one job.", "Foreground latency rises while the job runs.", "The job started around the degradation."],
    ["Identify the job and what it is doing.", "Check whether it is looping or processing an unexpected volume.", "Confirm it is safe to pause."],
    ["Pause or throttle the runaway job.", "Cap its resource budget so it cannot starve foreground work.", "Fix the input or logic that caused the runaway before re-enabling it."]],
  ["Load-balancer misroute", "Traffic is routed to unhealthy or wrong backends after a change.",
    ["A subset of requests fail while others succeed.", "Health checks disagree with reality.", "A routing or config change preceded the issue."],
    ["Compare the live routing config against the intended one.", "Check backend health-check definitions.", "Confirm which backends are actually receiving traffic."],
    ["Revert the routing change if it caused the misroute.", "Drain and replace genuinely unhealthy backends.", "Fix the health-check definition so it reflects real health."]],
  ["Data-export pipeline failure", "A scheduled customer or analytics export does not produce its output.",
    ["An export job is marked failed or is stuck.", "The expected output artifact is missing.", "Downstream consumers of the export are blocked."],
    ["Read the export job's error and last successful run.", "Check source-data availability and permissions.", "Confirm the destination is writable."],
    ["Re-run the export once the cause is fixed.", "Backfill any missed windows in order.", "Add a freshness alert so a silent failure is caught sooner."]],
  ["Security alert during an incident", "A security signal fires while an operational incident is in progress.",
    ["An unexpected access pattern or alert appears.", "The signal may or may not relate to the incident.", "It arrives amid the operational response."],
    ["Do not dismiss it as noise; capture it for the security team.", "Assess whether it changes the incident's severity.", "Preserve evidence rather than wiping affected systems."],
    ["Engage the security on-call in parallel with the operational response.", "Contain rather than destroy if compromise is suspected.", "Let security drive any breach-notification assessment."]],
  ["Elevated latency with no obvious cause", "Requests are slow across a service but nothing is clearly broken.",
    ["Latency is up while error rate is normal.", "No single dependency looks unhealthy.", "Throughput is roughly unchanged."],
    ["Check saturation of CPU, memory, threads, and connection pools.", "Look for a slow dependency hidden in the tail.", "Confirm no noisy-neighbour effect on shared infrastructure."],
    ["Relieve the saturated resource, scaling out if needed.", "Add capacity to a starved pool.", "If a neighbour is the cause, isolate the workload."]],
  ["Partial region degradation", "One region is unhealthy while others are fine.",
    ["Errors or latency are concentrated in a single region.", "Other regions serve normally.", "A regional dependency or change is suspected."],
    ["Confirm the impact is genuinely region-scoped.", "Check for a region-local change or provider issue.", "Verify cross-region failover is available and healthy."],
    ["Fail the affected region's traffic over to a healthy region if safe.", "Address the region-local cause once traffic is protected.", "Fail back carefully after confirming recovery."]],
  ["Corrupted or unexpected input data", "Malformed input causes downstream processing to fail or misbehave.",
    ["A stage rejects or mishandles records.", "The problem started when a new data source or format appeared.", "Reprocessing the same input reproduces it."],
    ["Identify the offending records and their source.", "Confirm whether validation should have caught them.", "Check whether the input contract changed upstream."],
    ["Quarantine the bad input so the pipeline can proceed.", "Tighten validation to reject rather than corrupt.", "Coordinate a fix with the upstream data producer."]],
];
const RUNBOOK_ALERTS = [
  ["INGEST_QUEUE_HIGH", "Ingest queue depth is above the high watermark", "Check downstream consumers first, then scale ingest workers."],
  ["INGEST_CLIENT_FLOOD", "A single client is submitting batches far above its norm", "Throttle or block the client key at the edge."],
  ["PLANNER_TAIL_LATENCY", "Planner tail latency exceeds its budget", "Check for connection-pool contention with reporting."],
  ["PLANNER_SERIAL_FALLBACK", "Planner has fallen back to serial planning", "Investigate database connection availability on the live path."],
  ["INVENTORY_DELTA", "Reconciliation delta is growing", "Check for a stalled pipeline stage or stuck reservations."],
  ["RESERVATION_LEAK", "Reservations are not expiring as expected", "Confirm the reservation TTL processor is running."],
  ["API_5XX_RATE", "API 5xx error rate above threshold", "Isolate the failing endpoint or dependency; consider rollback."],
  ["API_LATENCY_P99", "API p99 latency elevated", "Check dependencies and recent deploys."],
  ["RATE_LIMIT_SPIKE", "A surge of 429 responses", "Confirm it is genuine load and not a limiter misconfiguration."],
  ["ROBOT_ZONE_OFFLINE", "Many robots offline in one zone", "Suspect network or power in that zone; engage facilities."],
  ["FIRMWARE_ROLLOUT_ERRORS", "Errors correlate with a firmware version", "Halt the rollout and roll affected robots back."],
  ["REPLICA_SATURATION", "Reporting replica CPU or IO saturated", "Shed or kill heavy queries; verify pool isolation."],
  ["EVENTLOG_UNAVAILABLE", "The event log cannot be read or written", "Determine provider outage versus credentials; buffer if possible."],
  ["SSO_TOKEN_FAILURE", "Token issuance failing", "Check the identity provider and signing certificate."],
  ["CERT_EXPIRY_SOON", "A certificate expires soon", "Renew and deploy before the not-after date."],
  ["DISK_USAGE_HIGH", "A volume is nearly full", "Free space, then grow the volume if growth is legitimate."],
  ["DEPLOY_REGRESSION", "Health degraded right after a deploy", "Roll back first, investigate second."],
  ["HEARTBEAT_GAP", "A service stopped sending heartbeats", "Check the process and its host before assuming a wider outage."],
  ["BACKUP_FAILED", "A scheduled backup did not complete", "Re-run the backup and investigate the cause before the next window."],
  ["QUEUE_STUCK", "A background task queue is not draining", "Check the worker pool and for a poison message."],
];
const RUNBOOK_DEPS = [
  ["ingest service", "receives floor events; depends on the event log and downstream consumers."],
  ["planner", "assigns routes; depends on the inventory service and its dedicated database pool."],
  ["inventory service", "authoritative stock; depends on the primary PostgreSQL cluster."],
  ["reporting replica", "serves analytics; must stay isolated from the live picking path."],
  ["event log", "append-only history in the object store; source of truth for replay."],
  ["public API", "external surface; depends on auth, inventory, and the rate limiter."],
  ["auth / SSO", "issues tokens; a hard dependency for interactive and service calls."],
  ["object store", "durable storage for the event log and exports."],
  ["maintenance network", "carries firmware and robot control traffic per zone."],
  ["observability backend", "receives metrics, logs, and traces; degraded observability slows every incident."],
];
const RUNBOOK_COMMS = [
  ["Initial external update (Sev-1)", "We are investigating an issue affecting order fulfillment for some customers. Our team is engaged and we will post another update shortly. No action is needed from you at this time."],
  ["Follow-up external update", "We have identified the cause of the fulfillment delays and are applying a fix. Some orders may be processed more slowly than usual until the fix is fully in effect. Thank you for your patience."],
  ["Mitigated external update", "A fix has been applied and fulfillment is recovering. We are monitoring closely and will confirm full resolution once we are confident the issue is resolved. No customer action is required."],
  ["Resolved external update", "This incident is resolved. Fulfillment has returned to normal and no data was lost. We will publish a summary once our review is complete. Thank you for bearing with us."],
  ["Initial internal update", "Sev-1 declared for degraded fulfillment. Incident commander and communications lead are assigned. Current hypothesis and owner are in the channel. Next update in fifteen minutes or sooner if the picture changes."],
  ["Internal handover note", "Handing over incident command. Current state, actions taken, hypotheses ruled in and out, and the immediate next step are pinned in the channel. Communications lead is unchanged. The action log is up to date."],
];
const RUNBOOK_DIAGNOSTICS = [
  ["Confirm the blast radius", "before diving in, establish which customers, warehouses, or services are affected, and which are not."],
  ["Check recent changes", "list deploys, config changes, and infrastructure events in the last hour; a coincident change is the most common cause."],
  ["Read the golden signals", "look at latency, traffic, errors, and saturation for the suspect service before drilling into logs."],
  ["Follow the dependency chain", "trace from the failing symptom toward its upstream dependencies to find the true cause."],
  ["Inspect queue depths", "rising queues point to a downstream that has stalled or slowed."],
  ["Check connection-pool utilization", "pool exhaustion on the live path is a recurring cause of latency spikes."],
  ["Compare a healthy peer", "diff a healthy instance or zone against the unhealthy one to isolate what differs."],
  ["Verify certificate validity", "an expired certificate produces sudden, total failures on an otherwise-healthy endpoint."],
  ["Check clock skew", "skewed clocks silently break token validation and event ordering."],
  ["Look for a poison message", "a single malformed message can stall a whole consumer group."],
  ["Confirm pool isolation", "verify the live picking path is not sharing a connection pool with reporting."],
  ["Check disk and inode headroom", "a full volume or exhausted inodes crashes services in confusing ways."],
  ["Review the error budget", "decide how aggressively to intervene based on how much budget remains."],
  ["Capture evidence early", "snapshot logs, metrics, and state before you change anything, for the review and for security."],
  ["Test the rollback path", "confirm you can roll back the suspect change before you need to."],
  ["Validate the fix on one instance", "apply a change to a single instance and confirm recovery before rolling it out widely."],
  ["Watch for secondary failures", "a fix under load can trigger a new bottleneck; keep watching after you act."],
  ["Confirm data integrity", "for any incident touching stored data, verify integrity before declaring resolution."],
  ["Check replication lag", "before a failover, know which replica is most caught up."],
  ["Re-run health checks", "confirm health checks reflect reality and are not themselves misconfigured."],
  ["Narrow the time window", "pin the exact minute impact began; it usually lines up with a change or event."],
  ["Bisect the change set", "if several changes landed together, isolate which one by reverting or disabling them one at a time."],
  ["Check upstream provider status", "rule in or out a third-party outage before spending time on your own stack."],
  ["Inspect thread and connection pools", "saturated pools produce latency that looks like a downstream problem but is not."],
  ["Look at the slowest requests", "the tail often reveals the cause that averages hide."],
  ["Confirm feature-flag state", "a flag flipped at the wrong scale can degrade a service without any deploy."],
  ["Check for retry storms", "aggressive client retries can turn a small blip into a self-sustaining overload."],
  ["Validate config actually loaded", "confirm the running process is using the config you think it is."],
];
function buildRunbookBlocks() {
  const b = [];
  const H1 = "Northwind Robotics — Major-Incident Runbook";
  b.push({ h1: H1 });
  b.push({ p: "This runbook is the first thing an on-call engineer opens when paged for a potential major incident. It defines the severity model, the incident roles, how we communicate, a bank of service-specific playbooks, an alert reference, and the checklists we use to open and close an incident. Read the top sections now, before you are paged, so that under pressure you only need the playbook and the checklists." });
  b.push({ h2: "How to use this runbook" });
  b.push({ p: "When you are paged, first classify the severity, then declare an incident if it qualifies, then open the incident channel and assign roles. Only after that do you dive into the relevant service playbook. The single most common mistake under pressure is to start fixing before declaring and communicating; resist it." });
  b.push({ h2: "Severity levels" });
  b.push({ p: "Sev-1 is a full customer-facing outage or data-integrity risk. Sev-2 is significant degraded service with a workaround. Sev-3 is a minor issue with limited or no customer impact. When in doubt, declare the higher severity; it is cheap to downgrade and expensive to under-respond." });
  b.push({ li: "Sev-1 — customer-facing outage or data at risk; all-hands response, executive awareness." });
  b.push({ li: "Sev-2 — degraded service or a major feature down with a workaround; focused response." });
  b.push({ li: "Sev-3 — minor or internal-only impact; handled in normal working hours." });
  b.push({ h2: "Incident roles" });
  b.push({ p: "Every declared incident has, at minimum, an incident commander and a communications lead. The incident commander owns the response and makes the calls; they do not fix things themselves. The communications lead owns updates to stakeholders and the status page. On larger incidents we also add an operations lead and a scribe." });
  b.push({ li: "Incident commander — coordinates the response and owns decisions; delegates the hands-on work." });
  b.push({ li: "Communications lead — owns stakeholder updates and the public status page." });
  b.push({ li: "Operations lead — directs the hands-on remediation work." });
  b.push({ li: "Scribe — keeps a timestamped log of actions and decisions for the review." });
  b.push({ h2: "Communications" });
  b.push({ p: "Clear, frequent communication is the difference between a well-run incident and a chaotic one. The communications lead posts updates on a fixed rhythm so stakeholders never have to ask for status. Internal updates go to the incident channel; external updates go to the public status page in plain, non-technical language." });
  // Buried unique answer line (single paragraph => single physical line; TXT is not wrapped).
  b.push({ p: "Post a public status-page update within twenty minutes of declaring a Sev-1, and refresh it at least every thirty minutes until the incident is resolved." });
  b.push({ p: "Do not speculate about causes in external updates, never share customer data, and always say what customers should do, even if that is simply to wait. The incident commander approves every external update before it is posted." });
  b.push({ h2: "Escalation timeline" });
  b.push({ p: "Pages escalate automatically if they are not acknowledged. If the primary on-call does not acknowledge within ten minutes, the page goes to the secondary. If the secondary does not acknowledge within a further twenty minutes, it escalates to the engineering manager. Keep the incident channel updated at every step so escalation is never a surprise." });
  b.push({ h2: "Service playbooks" });
  b.push({ p: "Each playbook below covers one common failure mode: how it looks, what to check, and how to remediate. Playbooks are starting points, not scripts; use judgement, and if two playbooks seem to apply, treat the upstream cause as the primary incident." });
  for (const [title, impact, symptoms, checks, remediation] of RUNBOOK_PLAYBOOKS) {
    b.push({ h3: title });
    b.push({ p: "Impact. " + impact + " Classify the severity from the customer impact, not from how alarming the internal signals look." });
    b.push({ p: "Detection. This failure mode usually announces itself through one or more of the following signals; the more of them you see together, the more confident you can be:" });
    for (const s of symptoms) b.push({ li: s });
    b.push({ p: "Triage. Work through these checks in order before you change anything. The aim is to find the upstream cause rather than to treat a downstream symptom, because treating symptoms tends to prolong incidents:" });
    for (const c of checks) b.push({ li: c });
    b.push({ p: "Remediation. Once you have identified the cause, apply the smallest change that restores service, and prefer a reversible action over a clever one:" });
    for (const rr of remediation) b.push({ li: rr });
    b.push({ p: "Verification. After remediating, do not stand down immediately. Confirm the primary signal has recovered, confirm no secondary bottleneck has appeared, and confirm any data touched by the incident is consistent. Only then consider the incident mitigated." });
    b.push({ p: "Rollback and escalation. If remediation does not restore service within a reasonable window, escalate per the timeline above and prepare to roll back the most recent change on this path. Keep the incident channel updated so the incident commander can decide whether to widen the response or pull in another team." });
    b.push({ p: "Follow-up. Record a timeline, note which mitigations were temporary, and open follow-ups so this failure is less likely and less severe next time. A playbook that had to be improvised is itself a follow-up: update this document." });
  }
  b.push({ h2: "Communication templates" });
  b.push({ p: "Use these as starting points, not scripts. Fill in the specifics, keep external updates free of jargon and speculation, and always have the incident commander approve an external update before it is posted." });
  for (const [label, text] of RUNBOOK_COMMS) { b.push({ h3: label }); b.push({ p: text }); }
  b.push({ h2: "Diagnostic reference" });
  b.push({ p: "A checklist of high-value diagnostic moves that apply across many incidents. When you are not sure where to look, work down this list." });
  for (const [name, how] of RUNBOOK_DIAGNOSTICS) b.push({ li: `${name} — ${how}` });
  b.push({ h2: "Alert reference" });
  b.push({ p: "The alerts below page or notify on-call. For each, the immediate first response is given; the relevant playbook has the full procedure." });
  for (const [name, meaning, first] of RUNBOOK_ALERTS) b.push({ li: `${name}: ${meaning}. First response: ${first}` });
  b.push({ h2: "Service dependency reference" });
  b.push({ p: "Knowing what depends on what lets you find the upstream cause quickly instead of chasing symptoms." });
  for (const [svc, dep] of RUNBOOK_DEPS) b.push({ li: `${svc} — ${dep}` });
  b.push({ h2: "On-call preparation" });
  b.push({ p: "The best incident response happens before the page. When you go on call, confirm you can receive pages, that your access to production tooling and the incident channel works, and that you know where this runbook lives. A five-minute check at the start of a shift saves precious minutes later." });
  b.push({ p: "Keep a personal on-call kit: a charged phone, a way to get online quickly, and bookmarks to the dashboards, the status page, and the escalation contacts. Know who your secondary is and how to reach the incident commander of the week. If you are new to on-call, shadow an experienced engineer through at least one real or simulated incident first." });
  b.push({ p: "Rest matters. If you are too tired or unwell to respond effectively, hand over early rather than muddling through. There is no heroism in a slow, error-prone response; there is a lot of value in a clear-headed one." });
  b.push({ h2: "Severity decision examples" });
  b.push({ p: "Severity is judged by customer impact. These worked examples show how to classify quickly under pressure; when a case sits on a boundary, choose the higher severity." });
  for (const ex of [
    "A whole fulfillment center cannot pick orders: Sev-1, because customers are directly and broadly affected.",
    "The public API returns errors for all integrators: Sev-1, because external customers are unable to operate.",
    "Dashboards are stale but picking continues normally: Sev-3, because there is no direct customer impact.",
    "One robot is offline in an otherwise-healthy zone: Sev-3, handled in normal hours unless it spreads.",
    "Order throughput is degraded with a working slow path: Sev-2, because service is impaired but not down.",
    "A reporting replica is saturated but the live path is isolated and healthy: Sev-3, watched closely.",
    "Sign-in fails company-wide because SSO is down: Sev-1, because it blocks both staff and services.",
    "A single background export failed but can be re-run: Sev-3, tracked but not an emergency.",
  ]) b.push({ li: ex });
  b.push({ h2: "Incident glossary" });
  b.push({ p: "Shared vocabulary keeps a busy incident channel clear." });
  for (const [t, d] of [
    ["Incident commander", "the person coordinating the response and owning decisions; does not fix things personally."],
    ["Communications lead", "the person who owns stakeholder updates and the public status page."],
    ["Blast radius", "the set of customers, services, or data affected by an incident."],
    ["Mitigation", "an action that reduces or removes customer impact, even if the root cause remains."],
    ["Remediation", "the action that addresses the actual cause of the incident."],
    ["Rollback", "reverting a recent change to restore a known-good state."],
    ["Escalation", "bringing in additional people or authority when the current response is not enough."],
    ["Golden signals", "latency, traffic, errors, and saturation — the first metrics to check."],
    ["Error budget", "the tolerance for unreliability that guides how aggressively to intervene."],
    ["Backpressure", "a system slowing or rejecting input to protect itself from overload."],
    ["Poison message", "a single malformed item that repeatedly stalls a consumer."],
    ["Failover", "switching to a standby component when the primary is unavailable."],
    ["Replication lag", "how far behind a replica is from its primary."],
    ["Post-incident review", "the blameless review that turns an incident into durable improvements."],
    ["Standing down", "formally ending the active response once service is confirmed healthy."],
    ["Time in lieu", "compensating time off for hours worked responding out of hours."],
  ]) b.push({ li: `${t} — ${d}` });
  b.push({ h2: "Incident-open checklist" });
  b.push({ p: "Run through this list in the first few minutes of a declared incident." });
  for (const item of ["Classify the severity and declare the incident.", "Open the incident channel and pin the summary.", "Assign the incident commander and communications lead.", "Post the first status update within the communications deadline.", "Start a timestamped action log.", "Identify the most likely upstream cause before making changes."]) b.push({ li: item });
  b.push({ h2: "Incident-close checklist" });
  b.push({ p: "Before you stand down, confirm each of these." });
  for (const item of ["Service is confirmed healthy for at least one full check interval.", "A final status update has been posted and the status page is green.", "The action log and timeline are complete.", "Temporary mitigations are recorded with follow-ups to make them permanent or remove them.", "A post-incident review is scheduled with an owner."]) b.push({ li: item });
  b.push({ h2: "Post-incident review" });
  b.push({ p: "Every Sev-1 and Sev-2 gets a blameless review within a week. The review establishes the timeline, the contributing factors, and a small set of high-leverage follow-ups with owners and dates. The goal is to make the same failure less likely and less severe, not to assign blame." });
  return { path: "engineering/runbooks/major-incident-runbook.txt", format: "txt", title: H1, blocks: b };
}

// ---- 4) Information security policy (PDF via FODT) -----------------------
// A long policy is a catalogue of mandatory controls; each control expands via a
// realistic scaffold. The unique answer (a remediation SLA) is buried in the
// vulnerability-management control. Being a PDF, it is ALSO invisible to grep.
const SEC_CONTROLS = [
  ["AC-1", "Access control policy", "Access to Northwind systems and data is granted on the principle of least privilege and only for a legitimate business need.", ["access is granted by role rather than to individuals wherever possible", "every grant has an identifiable owner who is accountable for it", "access that is no longer needed is removed promptly"]],
  ["AC-2", "Account management", "User and service accounts are created, changed, and removed through a controlled, auditable process.", ["accounts are provisioned only after an approved request", "shared accounts are prohibited except where technically unavoidable and explicitly approved", "leavers' accounts are disabled on their last day"]],
  ["AC-3", "Privileged access", "Administrative access to production is tightly restricted, time-boxed, and fully logged.", ["privileged access requires a second approver", "elevated sessions are time-boxed and expire automatically", "every privileged action is logged and reviewed"]],
  ["AC-4", "Multi-factor authentication", "Interactive access to internal systems requires multi-factor authentication.", ["all staff enrol at least one strong second factor", "single-factor exceptions require documented risk acceptance", "phishing-resistant factors are preferred for privileged roles"]],
  ["AC-5", "Session management", "Interactive sessions are protected against hijacking and abandonment.", ["idle sessions time out automatically", "sessions are invalidated on sign-out and on credential change", "concurrent-session limits apply to privileged roles"]],
  ["IA-1", "Password standard", "Where passwords are used they meet a minimum strength standard and are never reused across systems.", ["minimum length and complexity are enforced by the identity provider", "known-breached passwords are rejected", "passwords are stored only as salted, hashed values"]],
  ["IA-2", "Secrets management", "Application secrets are stored in the approved secret manager, never in code or configuration files.", ["secrets are injected at runtime, not baked into images", "secrets are rotated on a schedule and after any suspected exposure", "access to secrets is scoped and logged"]],
  ["IA-3", "Key management", "Cryptographic keys are generated, stored, rotated, and destroyed under a documented lifecycle.", ["keys are stored in the managed key service", "keys are rotated on a regular schedule", "retired keys are destroyed once no longer needed for decryption"]],
  ["IA-4", "Certificate management", "TLS certificates are issued, tracked, and renewed before expiry through automation.", ["certificates are inventoried centrally", "renewal is automated with alerting well before expiry", "private keys never leave their intended host or service"]],
  ["CR-1", "Encryption in transit", "All data in transit over untrusted networks is encrypted with current, strong protocols.", ["TLS is required for all external and internal service traffic", "obsolete protocol versions and ciphers are disabled", "certificate validation is enforced, not bypassed"]],
  ["CR-2", "Encryption at rest", "Confidential and restricted data is encrypted at rest.", ["storage-level encryption is enabled on all data stores", "restricted datasets carry application-level encryption in addition", "encryption keys are managed under the key-management control"]],
  ["NW-1", "Network segmentation", "Networks are segmented so that a compromise in one zone does not grant free movement to others.", ["production, corporate, and maintenance networks are separated", "traffic between segments is default-deny and explicitly allowed", "the robot maintenance network is isolated from the corporate network"]],
  ["NW-2", "Firewall and edge control", "Inbound and outbound network access is controlled at well-defined boundaries.", ["only required ports and destinations are permitted", "rules are reviewed periodically and stale rules removed", "changes to edge rules follow the change-management control"]],
  ["NW-3", "Remote access", "Remote access to internal systems is authenticated, encrypted, and limited to what is needed.", ["remote access uses the approved VPN or zero-trust gateway", "device posture is checked before access is granted", "split-tunnel exceptions require approval"]],
  ["SD-1", "Secure development lifecycle", "Security is built into how software is designed, built, and released.", ["threat modelling is applied to significant new features", "security requirements are captured alongside functional ones", "releases pass automated security checks before production"]],
  ["SD-2", "Code review", "Code changes are peer-reviewed before they reach production.", ["every change requires at least one independent approval", "security-sensitive changes get an additional security review", "review comments are resolved, not dismissed silently"]],
  ["SD-3", "Dependency management", "Third-party dependencies are inventoried and kept free of known vulnerabilities.", ["a software bill of materials is maintained per service", "dependencies are scanned continuously for known issues", "unmaintained or high-risk dependencies are replaced"]],
  ["VM-1", "Vulnerability management", "Vulnerabilities are discovered, prioritized by severity, and remediated within defined timeframes.", ["automated scanning runs continuously across systems and images", "findings are triaged and assigned an owner", "remediation deadlines are tracked to closure"]],
  ["VM-2", "Penetration testing", "Independent testing validates the effectiveness of controls.", ["external penetration tests are performed on a regular cadence", "significant findings feed the vulnerability-management process", "retests confirm that fixes are effective"]],
  ["CM-1", "Patch management", "Systems and software are kept current with security patches.", ["security patches are applied within severity-based windows", "unattended systems are covered by automated patching", "exceptions are documented with compensating controls"]],
  ["CM-2", "Change management", "Changes to production are controlled, reviewed, and reversible.", ["changes are peer-reviewed and recorded", "high-risk changes have a rollback plan and a change window", "emergency changes are reviewed retrospectively"]],
  ["CM-3", "Configuration and hardening", "Systems are configured to a documented secure baseline.", ["hardening baselines are defined for each platform", "configuration is managed as code and drift is detected", "unused services and default credentials are removed"]],
  ["EP-1", "Endpoint protection", "Employee and server endpoints run approved protection and are centrally managed.", ["endpoints run the approved security agent", "disk encryption is enforced on all laptops", "endpoints out of compliance are flagged and remediated"]],
  ["EP-2", "Mobile device management", "Mobile devices accessing company data are enrolled and controlled.", ["company data is accessed only from enrolled devices", "a lost device can be remotely wiped of company data", "a screen lock and encryption are enforced"]],
  ["EP-3", "Email and phishing defence", "Email is filtered and staff are prepared to recognize phishing.", ["inbound mail is scanned for malware and spoofing", "external mail is clearly marked", "staff can report suspected phishing with one click"]],
  ["DP-1", "Data classification", "Information is classified so that it is protected in proportion to its sensitivity.", ["data is classified as public, internal, confidential, or restricted", "the classification determines handling and access", "owners are responsible for classifying their data"]],
  ["DP-2", "Data retention", "Data is kept only as long as it is needed or legally required, then securely destroyed.", ["retention periods are defined per data category", "records past their retention are securely destroyed", "legal holds override normal retention when in force"]],
  ["DP-3", "Data loss prevention", "Controls reduce the risk of confidential data leaving the organization improperly.", ["egress of restricted data is monitored and controlled", "sharing of confidential data externally requires justification", "removable-media use is restricted"]],
  ["DP-4", "Privacy and data protection", "Personal data is processed lawfully, fairly, and only for legitimate purposes.", ["personal data is collected only where there is a lawful basis", "access to personal data is limited to those who need it", "data-subject requests are handled within legal timeframes"]],
  ["BC-1", "Backup and recovery", "Critical data is backed up and recoveries are tested.", ["backups run on a defined schedule with monitored success", "backups are stored durably and, for critical data, off-site", "restores are tested regularly, not assumed to work"]],
  ["BC-2", "Business continuity", "The business can continue operating through significant disruptions.", ["continuity plans exist for critical services", "recovery objectives are defined and reviewed", "plans are exercised and updated after each exercise"]],
  ["BC-3", "Disaster recovery", "Critical systems can be recovered within their defined objectives after a disaster.", ["recovery-time and recovery-point objectives are documented", "recovery procedures are kept current", "disaster-recovery exercises validate the procedures"]],
  ["LM-1", "Logging and monitoring", "Security-relevant events are logged, retained, and monitored.", ["systems emit security-relevant logs to a central store", "logs are protected from tampering", "logs are monitored for signs of compromise"]],
  ["LM-2", "Security event management", "Alerts are triaged and investigated by defined owners.", ["security alerts are routed to an accountable team", "alerts are triaged against a documented severity model", "investigations and outcomes are recorded"]],
  ["IR-1", "Incident response", "Security incidents are detected, contained, eradicated, and reviewed.", ["an incident-response plan defines roles and steps", "incidents are contained before eradication and recovery", "every incident gets a blameless review with follow-ups"]],
  ["IR-2", "Breach notification", "Where required, affected parties and regulators are notified promptly.", ["notification obligations are understood in advance", "a breach is assessed against those obligations quickly", "notifications are made within the required timeframes"]],
  ["TP-1", "Vendor risk management", "Third parties that handle Northwind data are assessed and monitored.", ["vendors are risk-assessed before onboarding", "contracts include appropriate security and privacy terms", "vendor access is reviewed periodically"]],
  ["TP-2", "Third-party access", "External parties are granted only the access they need, for only as long as they need it.", ["third-party access is time-boxed and least-privilege", "external accounts are clearly identifiable", "access is revoked at the end of the engagement"]],
  ["PS-1", "Physical security", "Facilities and hardware are protected against unauthorized physical access.", ["access to offices and labs is badge-controlled", "visitors are signed in and escorted near active robots", "server and network equipment is kept in secured areas"]],
  ["PS-2", "Media handling and disposal", "Storage media are handled and disposed of so that data cannot be recovered improperly.", ["media carrying confidential data are tracked", "media are securely wiped or destroyed before disposal", "disposal is documented for restricted data"]],
  ["HR-1", "Personnel security", "Security responsibilities are built into the employment lifecycle.", ["background checks are performed where lawful and appropriate", "confidentiality obligations are agreed on joining", "access is adjusted promptly on role change or exit"]],
  ["HR-2", "Security awareness training", "Staff are trained to understand and meet their security responsibilities.", ["all staff complete security training on joining and annually", "role-specific training is provided where needed", "training completion is tracked"]],
  ["HR-3", "Acceptable use", "Company systems and equipment are used appropriately and lawfully.", ["the acceptable-use policy governs use of company systems", "incidental personal use is kept reasonable and lawful", "misuse is handled under the disciplinary process"]],
  ["CP-1", "Compliance and audit", "The security programme is measured against its obligations and improved.", ["controls are audited on a defined cadence", "findings are tracked to closure", "the policy is updated as obligations and risks change"]],
];
const SEC_DEFS = [
  ["Confidentiality", "ensuring information is accessible only to those authorized to have access."],
  ["Integrity", "safeguarding the accuracy and completeness of information and processing methods."],
  ["Availability", "ensuring authorized users have access to information and systems when required."],
  ["Least privilege", "granting only the minimum access necessary to perform a task."],
  ["Restricted data", "the most sensitive classification, requiring the strongest handling and encryption."],
  ["Confidential data", "sensitive information whose disclosure could harm Northwind or its customers."],
  ["Internal data", "information intended for use within Northwind but not publicly disclosed."],
  ["Public data", "information approved for public release."],
  ["Control owner", "the person accountable for a control operating effectively."],
  ["Compensating control", "an alternative safeguard used when a primary control cannot be met."],
  ["Exception", "a documented, time-boxed, and approved deviation from a control."],
  ["Recovery-time objective", "the target time to restore a service after a disruption."],
  ["Recovery-point objective", "the maximum acceptable amount of data loss measured in time."],
  ["Threat model", "a structured analysis of how a system could be attacked and how it is defended."],
  ["Software bill of materials", "an inventory of the components and dependencies in a piece of software."],
];
function buildSecurityBlocks() {
  const b = [];
  const H1 = "Northwind Robotics — Information Security Policy";
  b.push({ h1: H1 });
  b.push({ p: "This Information Security Policy defines how Northwind Robotics protects the confidentiality, integrity, and availability of the information and systems that run its autonomous fulfillment platform. It applies to every employee, contractor, and third party that accesses Northwind systems or data, regardless of where the system is hosted or where the person is working." });
  b.push({ p: "The policy is organized as a catalogue of mandatory controls. Each control states its purpose, the systems and people it applies to, its specific requirements, how it is implemented, and how compliance is verified. Controls are cumulative: satisfying one does not excuse non-compliance with another." });
  b.push({ p: "Northwind treats security as an engineering discipline. Controls are automated and enforced in code wherever possible, exceptions are documented and time-boxed, and every control is reviewed on a fixed cadence and after any material change or related incident." });
  b.push({ h2: "Scope and applicability" });
  b.push({ p: "This policy covers all production, staging, and corporate systems, all customer and employee data, and all networks operated by or on behalf of Northwind Robotics. Where a customer contract or applicable law imposes a stricter requirement than this policy, the stricter requirement governs." });
  b.push({ h2: "Roles and responsibilities" });
  b.push({ p: "The Chief Information Security Officer owns this policy and approves exceptions. Engineering managers are accountable for the controls that apply to the systems they operate. Every individual is responsible for completing security training, protecting the credentials and data entrusted to them, and reporting suspected incidents promptly." });
  b.push({ p: "A security review board meets monthly to review exceptions, incidents, and proposed changes to this policy, and its decisions are recorded in the governance register." });
  b.push({ h2: "Security controls" });
  for (const [id, title, purpose, reqs] of SEC_CONTROLS) {
    b.push({ h3: `${id} — ${title}` });
    b.push({ p: purpose });
    b.push({ p: `This control applies to all Northwind Robotics production and corporate systems in scope, and compliance is mandatory unless a documented, time-boxed exception has been approved by the Chief Information Security Officer.` });
    b.push({ p: "Requirements. " + reqs.map((r, i) => (i === 0 ? "First, " : i === 1 ? "second, " : i === 2 ? "third, " : "and additionally, ") + r).join("; ") + "." });
    b.push({ p: "Implementation is enforced through configuration and tooling managed as code, so that the control is applied consistently and any change is peer-reviewed and logged before it takes effect in production." });
    b.push({ p: "Compliance is assessed on a defined cadence through automated checks and a documented review, and the resulting evidence is retained in the governance register so that internal and external auditors can confirm the control is operating effectively." });
    if (id === "VM-1") {
      // Buried unique answer line — short so pdftotext keeps it on one line.
      b.push({ p: "Critical vulnerabilities must be remediated within seven days of confirmed disclosure." });
      b.push({ p: "High-severity findings are remediated within thirty days, and medium and low findings are scheduled into the normal maintenance cycle. Every remediation deadline starts when a finding is confirmed, not when it is first reported, and slipping a deadline requires documented risk acceptance." });
    }
  }
  b.push({ h2: "Appendix A — Definitions" });
  b.push({ p: "The terms below are used throughout this policy." });
  for (const [t, d] of SEC_DEFS) b.push({ li: `${t} — ${d}` });
  b.push({ h2: "Appendix B — Review and exceptions" });
  b.push({ p: "This policy is reviewed at least annually and whenever a material change to the environment, the business, or the threat landscape warrants it. Exceptions to any control must be requested in writing, carry a business justification and a compensating control, be time-boxed, and be approved by the Chief Information Security Officer. Expired exceptions are treated as non-compliance." });
  return { path: "security/information-security-policy.pdf", format: "pdf", title: H1, blocks: b };
}

// The four large documents, materialised by build().
const LARGE_DOCS = [buildHandbookBlocks(), buildApiRefBlocks(), buildRunbookBlocks(), buildSecurityBlocks()];

// ---------------------------------------------------------------------------
// Ground-truth query set. Distribution per SPEC (REVISION 2 — honesty fix):
//   binary_only   (answer ONLY in a pdf/docx)                      — 7
//   large_literal (answer buried in a >=60 KB doc; both can hit,   — 4
//                  but the baseline must open the whole large file)
//   robustness    (answer worded differently; a FAIR grep of the   — 5
//                  question's own tokens can TIE — not a semantic win)
//   literal       (plain string in a small html/md/txt; both find) — 6
// keywords = curated terms the baseline greps (UNION'd with question tokens).
// expect_substring = the ground-truth answer text (case-insensitive check).
// ---------------------------------------------------------------------------
const QUERIES = [
  // ---------- binary_only (answer lives ONLY in a pdf/docx) ----------
  {
    id: "q01",
    question: "How many weeks of paid parental leave do employees receive?",
    keywords: ["parental leave", "paid leave"],
    expect_substring: "16 weeks",
    answer_path: "corpus/hr/handbook.pdf",
    answer_format: "pdf",
    match_type: "binary_only",
  },
  {
    id: "q02",
    question: "How many days of paid time off do full-time employees accrue per year?",
    keywords: ["paid time off", "PTO"],
    expect_substring: "22 days",
    answer_path: "corpus/hr/handbook.pdf",
    answer_format: "pdf",
    match_type: "binary_only",
  },
  {
    id: "q03",
    question: "How many days of bereavement leave are provided per event?",
    keywords: ["bereavement leave", "bereavement"],
    expect_substring: "5 working days",
    answer_path: "corpus/hr/benefits/leave-policy.docx",
    answer_format: "docx",
    match_type: "binary_only",
  },
  {
    id: "q04",
    question: "After how long does an unacknowledged page escalate to the secondary on-call engineer?",
    keywords: ["escalation", "secondary on-call"],
    expect_substring: "15 minutes",
    answer_path: "corpus/engineering/oncall-runbook.docx",
    answer_format: "docx",
    match_type: "binary_only",
  },
  {
    id: "q05",
    question: "What is the acknowledgement SLA for a Sev-1 incident?",
    keywords: ["Sev-1", "acknowledge"],
    expect_substring: "within 5 minutes",
    answer_path: "corpus/engineering/oncall-runbook.docx",
    answer_format: "docx",
    match_type: "binary_only",
  },
  {
    id: "q06",
    question: "How long must customer transaction records be retained?",
    keywords: ["retention", "retained"],
    expect_substring: "7 years",
    answer_path: "corpus/security/data-classification.pdf",
    answer_format: "pdf",
    match_type: "binary_only",
  },
  {
    id: "q07",
    question: "What is the domestic meal per diem rate?",
    keywords: ["per diem", "meal allowance"],
    expect_substring: "$75 per day",
    answer_path: "corpus/finance/expense-policy.pdf",
    answer_format: "pdf",
    match_type: "binary_only",
  },

  // ---------- large_literal (answer buried deep in a >=60 KB document) ----------
  // A FAIR baseline CAN grep these answer lines, but to read/verify the answer it
  // must open the whole large file (context blowup); XERJ returns one ranked
  // passage. ll04 additionally lives in a PDF, so grep is doubly blind there.
  {
    id: "ll01",
    question: "How much is the employee referral bonus for a successful hire?",
    keywords: ["referral bonus", "referral"],
    expect_substring: "$3,000 per successful hire",
    answer_path: "corpus/hr/employee-handbook.html",
    answer_format: "html",
    match_type: "large_literal",
  },
  {
    id: "ll02",
    question: "What is the default value of the ingest max_batch_size parameter?",
    keywords: ["max_batch_size", "batch size"],
    expect_substring: "defaults to 4096 records",
    answer_path: "corpus/engineering/api-reference.md",
    answer_format: "md",
    match_type: "large_literal",
  },
  {
    id: "ll03",
    question: "How quickly must a public status-page update be posted after declaring a Sev-1?",
    keywords: ["status page", "status-page update"],
    expect_substring: "within twenty minutes",
    answer_path: "corpus/engineering/runbooks/major-incident-runbook.txt",
    answer_format: "txt",
    match_type: "large_literal",
  },
  {
    id: "ll04",
    question: "Within how many days must critical vulnerabilities be remediated?",
    keywords: ["vulnerability remediation", "critical vulnerabilities"],
    expect_substring: "within seven days",
    answer_path: "corpus/security/information-security-policy.pdf",
    answer_format: "pdf",
    match_type: "large_literal",
  },

  // ---------- robustness (answer phrased differently; FAIR grep of the question's
  // own tokens can TIE — kept to show single-query convenience, NOT a semantic win) ----------
  {
    id: "s01",
    question: "What is Northwind's policy on remote work versus coming into the office?",
    keywords: ["remote work", "telecommute"],
    expect_substring: "three days a week",
    answer_path: "corpus/operations/facilities.md",
    answer_format: "md",
    match_type: "robustness",
  },
  {
    id: "s02",
    question: "How does a user recover access to a locked account?",
    keywords: ["reset password", "forgot password"],
    expect_substring: "self-service portal",
    answer_path: "corpus/security/access-control.html",
    answer_format: "html",
    match_type: "robustness",
  },
  {
    id: "s03",
    question: "What developer hardware is issued to a newly hired engineer on their first day?",
    keywords: ["laptop", "company computer"],
    expect_substring: "16-inch developer workstation",
    answer_path: "corpus/engineering/onboarding.md",
    answer_format: "md",
    match_type: "robustness",
  },
  {
    id: "s04",
    question: "Which data store backs the inventory and SKU records?",
    keywords: ["database", "product catalog"],
    expect_substring: "PostgreSQL 15 cluster",
    answer_path: "corpus/engineering/architecture/system-overview.md",
    answer_format: "md",
    match_type: "robustness",
  },
  {
    id: "s05",
    question: "What can a customer do if they want to send a purchased unit back?",
    keywords: ["refund", "return policy"],
    expect_substring: "full reimbursement",
    answer_path: "corpus/product/faq.html",
    answer_format: "html",
    match_type: "robustness",
  },

  // ---------- literal (plain string in html/md/txt; BOTH approaches find) ----------
  {
    id: "l01",
    question: "What is the default API rate limit?",
    keywords: ["rate limit", "API rate limit"],
    expect_substring: "1000 requests per minute",
    answer_path: "corpus/engineering/architecture/data-pipeline.md",
    answer_format: "md",
    match_type: "literal",
  },
  {
    id: "l02",
    question: "What port does the ingest service listen on?",
    keywords: ["port", "ingest service"],
    expect_substring: "port 8412",
    answer_path: "corpus/engineering/architecture/system-overview.md",
    answer_format: "md",
    match_type: "literal",
  },
  {
    id: "l03",
    question: "When is the weekly engineering sync held?",
    keywords: ["weekly engineering sync", "engineering sync"],
    expect_substring: "Wednesday at 10:00",
    answer_path: "corpus/meetings/decision-log.md",
    answer_format: "md",
    match_type: "literal",
  },
  {
    id: "l04",
    question: "What uptime does the platform SLA guarantee?",
    keywords: ["uptime", "SLA"],
    expect_substring: "99.9% uptime",
    answer_path: "corpus/product/faq.html",
    answer_format: "html",
    match_type: "literal",
  },
  {
    id: "l05",
    question: "Who must approve purchases over the standard threshold?",
    keywords: ["approve", "purchase"],
    expect_substring: "Director of Operations",
    answer_path: "corpus/finance/procurement.html",
    answer_format: "html",
    match_type: "literal",
  },
  {
    id: "l06",
    question: "What is the Q3 target for warehouse pick accuracy?",
    keywords: ["pick accuracy", "warehouse"],
    expect_substring: "99.5% pick accuracy",
    answer_path: "corpus/product/roadmap.txt",
    answer_format: "txt",
    match_type: "literal",
  },
];

// ---------------------------------------------------------------------------
// FODT authoring + soffice conversion
// ---------------------------------------------------------------------------
function xmlEscape(s) {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

function blocksToFodt(blocks) {
  const parts = [];
  for (const b of blocks) {
    if (b.h1 != null) parts.push(`<text:h text:outline-level="1">${xmlEscape(b.h1)}</text:h>`);
    else if (b.h2 != null) parts.push(`<text:h text:outline-level="2">${xmlEscape(b.h2)}</text:h>`);
    else if (b.h3 != null) parts.push(`<text:h text:outline-level="3">${xmlEscape(b.h3)}</text:h>`);
    else if (b.p != null) parts.push(`<text:p>${xmlEscape(b.p)}</text:p>`);
    else if (b.li != null) parts.push(`<text:p>• ${xmlEscape(b.li)}</text:p>`);
    else if (b.code != null) parts.push(`<text:p>${xmlEscape(b.code)}</text:p>`);
  }
  return `<?xml version="1.0" encoding="UTF-8"?>
<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" office:version="1.3" office:mimetype="application/vnd.oasis.opendocument.text">
<office:body><office:text>
${parts.join("\n")}
</office:text></office:body>
</office:document>
`;
}

function soffice(args) {
  return execFileSync("soffice", ["--headless", SOFFICE_ENV, ...args], {
    stdio: ["ignore", "pipe", "pipe"],
    timeout: 120000,
  }).toString();
}

// Convert a list of source files to `fmt`, writing into outDir. Returns after
// verifying every expected output exists; retries any missing one-by-one.
function convertBatch(sources, convertTo, outDir, expectedExt) {
  fs.mkdirSync(outDir, { recursive: true });
  const expected = sources.map(
    (s) => path.join(outDir, path.basename(s).replace(/\.[^.]+$/, `.${expectedExt}`))
  );
  try {
    soffice(["--convert-to", convertTo, "--outdir", outDir, ...sources]);
  } catch (e) {
    // fall through to per-file retry below
  }
  for (let i = 0; i < sources.length; i++) {
    if (!fs.existsSync(expected[i])) {
      soffice(["--convert-to", convertTo, "--outdir", outDir, sources[i]]);
    }
    if (!fs.existsSync(expected[i])) {
      throw new Error(`soffice failed to produce ${expected[i]} from ${sources[i]}`);
    }
  }
  return expected;
}

// ---------------------------------------------------------------------------
// Extraction helpers — MUST mirror what xerj-index.mjs uses, so verification
// checks the exact text the indexer will see.
// ---------------------------------------------------------------------------
function extractPdf(file) {
  return execFileSync("pdftotext", ["-layout", file, "-"], { timeout: 60000 }).toString();
}
function extractDocx(file) {
  const out = path.join(BUILD, "docx-extract");
  fs.mkdirSync(out, { recursive: true });
  soffice(["--convert-to", "txt:Text", "--outdir", out, file]);
  const txt = path.join(out, path.basename(file).replace(/\.docx$/, ".txt"));
  return fs.readFileSync(txt, "utf8").replace(/^﻿/, "");
}
function stripHtml(html) {
  return html
    .replace(/<script[\s\S]*?<\/script>/gi, " ")
    .replace(/<style[\s\S]*?<\/style>/gi, " ")
    .replace(/<[^>]+>/g, " ")
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&#39;/g, "'")
    .replace(/&quot;/g, '"')
    .replace(/\s+/g, " ")
    .trim();
}
function extractedText(absPath, fmt) {
  if (fmt === "pdf") return extractPdf(absPath);
  if (fmt === "docx") return extractDocx(absPath);
  if (fmt === "html") return stripHtml(fs.readFileSync(absPath, "utf8"));
  return fs.readFileSync(absPath, "utf8"); // md / txt
}

const ci = (s) => s.toLowerCase();

// ---------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------
function build() {
  // Idempotent: wipe + recreate corpus/.
  fs.rmSync(CORPUS, { recursive: true, force: true });
  fs.mkdirSync(CORPUS, { recursive: true });

  // 1) Text files (small) + large TEXT documents (html/md/txt) rendered from blocks.
  for (const f of TEXT_FILES) {
    const abs = path.join(CORPUS, f.path);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, f.body);
  }
  for (const d of LARGE_DOCS) {
    if (d.format === "pdf") continue; // the large PDF is built via the binary path below
    const abs = path.join(CORPUS, d.path);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    const content =
      d.format === "html" ? renderHtml(d.title, d.blocks)
      : d.format === "md" ? renderMd(d.blocks)
      : renderTxt(d.blocks);
    fs.writeFileSync(abs, content);
  }

  // 2) Binary files via FODT -> soffice. The small BINARIES plus any large PDF docs.
  const binaries = [
    ...BINARIES,
    ...LARGE_DOCS.filter((d) => d.format === "pdf").map((d) => ({ path: d.path, format: "pdf", blocks: d.blocks })),
  ];
  const srcDir = path.join(BUILD, "fodt");
  fs.mkdirSync(srcDir, { recursive: true });
  // unique intermediate name per binary to avoid basename collisions
  const jobs = binaries.map((b, i) => {
    const stem = `bin${String(i).padStart(2, "0")}`;
    const fodt = path.join(srcDir, `${stem}.fodt`);
    fs.writeFileSync(fodt, blocksToFodt(b.blocks));
    return { ...b, fodt, stem };
  });

  for (const fmt of ["pdf", "docx"]) {
    const group = jobs.filter((j) => j.format === fmt);
    if (group.length === 0) continue;
    const outDir = path.join(BUILD, `out-${fmt}`);
    const produced = convertBatch(group.map((j) => j.fodt), fmt, outDir, fmt);
    group.forEach((j, idx) => {
      const dst = path.join(CORPUS, j.path);
      fs.mkdirSync(path.dirname(dst), { recursive: true });
      fs.copyFileSync(produced[idx], dst);
    });
  }

  // 3) queries.json
  fs.writeFileSync(QUERIES_JSON, JSON.stringify(QUERIES, null, 2) + "\n");
}

// ---------------------------------------------------------------------------
// Verify the ground-truth invariants that keep the comparison honest.
// Throws on any violation.
// ---------------------------------------------------------------------------
function listTextFiles() {
  // All non-binary files the grep baseline can actually read.
  const out = [];
  const walk = (dir) => {
    for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
      const p = path.join(dir, e.name);
      if (e.isDirectory()) walk(p);
      else if (/\.(html|md|txt)$/i.test(e.name)) out.push(p);
    }
  };
  walk(CORPUS);
  return out;
}

// --- FAIR-baseline term model (mirrors grep-baseline.mjs) ------------------
// The honest baseline greps the UNION of the curated keywords AND the salient
// content tokens of the QUESTION itself. We reproduce that here so the
// self-verify can assert that a fair grep really could (robustness/large_literal)
// or really could not (binary_only) reach an answer line — the exact fairness the
// audit demanded. Keep this in sync with grep-baseline.mjs.
const QUESTION_STOPWORDS = new Set([
  "a", "about", "above", "after", "again", "against", "all", "am", "an", "and", "any",
  "are", "as", "at", "be", "because", "been", "before", "being", "below", "between",
  "both", "but", "by", "can", "cannot", "could", "day", "did", "do", "does", "doing",
  "done", "down", "during", "each", "few", "for", "from", "further", "get", "gets",
  "getting", "got", "had", "has", "have", "having", "he", "her", "here", "hers", "him",
  "his", "how", "i", "if", "in", "into", "is", "it", "its", "let", "long", "many", "may",
  "me", "might", "more", "most", "much", "must", "my", "no", "nor", "not", "of", "off",
  "on", "once", "one", "only", "or", "other", "our", "ours", "out", "over", "own", "per",
  "same", "shall", "she", "should", "so", "some", "such", "than", "that", "the", "their",
  "theirs", "them", "then", "there", "these", "they", "this", "those", "through", "to",
  "too", "under", "until", "up", "upon", "us", "very", "versus", "vs", "was", "we",
  "were", "what", "when", "where", "which", "while", "who", "whom", "why", "will", "with",
  "would", "you", "your", "yours",
]);
function tokenizeQuestion(question) {
  return String(question)
    .toLowerCase()
    .replace(/['’]/g, " ")
    .match(/[a-z0-9](?:[a-z0-9%$.\-]*[a-z0-9%])?/g) || [];
}
function isSalientToken(tok) {
  if (!tok || QUESTION_STOPWORDS.has(tok)) return false;
  if (/[0-9%$]/.test(tok)) return true;
  return /^[a-z][a-z\-]*$/.test(tok) && tok.length >= 3;
}
function questionTerms(question) {
  if (typeof question !== "string" || question.trim() === "") return [];
  const terms = [];
  for (const m of question.matchAll(/["“”']([^"“”']+)["“”']/g)) {
    const phrase = m[1].trim();
    if (phrase) terms.push(phrase);
  }
  const toks = tokenizeQuestion(question);
  for (let i = 0; i < toks.length; i++) {
    if (isSalientToken(toks[i])) terms.push(toks[i]);
    if (i + 1 < toks.length && isSalientToken(toks[i]) && isSalientToken(toks[i + 1])) {
      terms.push(`${toks[i]} ${toks[i + 1]}`);
    }
  }
  return terms;
}
function fairTerms(q) {
  const union = [...(q.keywords || []), ...questionTerms(q.question)];
  const seen = new Set();
  const terms = [];
  for (const t of union) {
    const key = ci(t);
    if (!seen.has(key)) { seen.add(key); terms.push(t); }
  }
  return terms;
}
// Would a FAIR line-oriented grep of `terms` surface a line containing `sub`?
// (rg matches a line by any term; a "hit" needs that line to also hold the answer.)
function baselineLineHit(rawText, terms, sub) {
  const subLc = ci(sub);
  for (const raw of rawText.split(/\r?\n/)) {
    const ln = ci(raw);
    if (!ln.includes(subLc)) continue;
    if (terms.some((t) => ln.includes(ci(t)))) return true;
  }
  return false;
}

function verify() {
  const problems = [];
  const textFiles = listTextFiles();
  const textBlob = ci(textFiles.map((f) => fs.readFileSync(f, "utf8")).join("\n"));

  for (const q of QUERIES) {
    const abs = path.join(HERE, q.answer_path);
    if (!fs.existsSync(abs)) {
      problems.push(`[${q.id}] answer_path missing: ${q.answer_path}`);
      continue;
    }
    const text = extractedText(abs, q.answer_format);
    const sub = ci(q.expect_substring);

    // (A) The answer substring must appear in the extracted text of its file,
    //     using the SAME extraction the indexer uses.
    if (!ci(text).includes(sub)) {
      problems.push(`[${q.id}] expect_substring "${q.expect_substring}" NOT in extracted ${q.answer_path}`);
    }

    if (q.match_type === "binary_only") {
      // (B) The answer must live ONLY in the binary: the substring must not
      //     appear in ANY text file (else the grep baseline could find it).
      if (textBlob.includes(sub)) {
        problems.push(`[${q.id}] binary_only substring "${q.expect_substring}" leaked into a text file`);
      }
      if (!(q.answer_format === "pdf" || q.answer_format === "docx")) {
        problems.push(`[${q.id}] binary_only answer_format must be pdf/docx, got ${q.answer_format}`);
      }
    } else if (q.match_type === "large_literal") {
      // (C) Context-efficiency demonstrator. The source file must be genuinely
      //     LARGE (>=60 KB) so that "load the whole file" >> "one ranked passage",
      //     and the answer must be greppable in the extracted plaintext (check A
      //     already asserts that). For non-binary formats, a FAIR baseline must be
      //     able to reach the line (both approaches HIT — that is the point); for a
      //     pdf/docx it is additionally invisible to grep, which is fine.
      const bytes = fs.statSync(abs).size;
      if (bytes < MIN_LARGE_BYTES) {
        problems.push(`[${q.id}] large_literal source ${q.answer_path} is ${bytes} B (< ${MIN_LARGE_BYTES} B floor)`);
      }
      if (!(q.answer_format === "pdf" || q.answer_format === "docx")) {
        const raw = fs.readFileSync(abs, "utf8");
        if (!baselineLineHit(raw, fairTerms(q), q.expect_substring)) {
          problems.push(`[${q.id}] large_literal: a FAIR baseline could not reach "${q.expect_substring}" in ${q.answer_path}`);
        }
      }
    } else if (q.match_type === "robustness") {
      // (D) Formerly "semantic_only". The HONEST finding: a diligent grep of the
      //     QUESTION's own tokens can TIE here (lexical overlap, not deep meaning).
      //     So we assert the OPPOSITE of the old rigged check: the answer line MUST
      //     be reachable by the question's own salient tokens (never rigged to miss).
      const raw = fs.readFileSync(abs, "utf8");
      if (!baselineLineHit(raw, questionTerms(q.question), q.expect_substring)) {
        problems.push(`[${q.id}] robustness: answer "${q.expect_substring}" is NOT reachable by the question's own tokens in ${q.answer_path} (would be a rigged "semantic win")`);
      }
      if (q.answer_format === "pdf" || q.answer_format === "docx") {
        problems.push(`[${q.id}] robustness should live in html/md/txt, got ${q.answer_format}`);
      }
    } else if (q.match_type === "literal") {
      // (D) The baseline greps keywords and a hit needs a matched LINE that also
      //     contains the substring. Require >=1 keyword co-occurring with the
      //     substring on a single raw line of the answer file.
      const rawLines = fs.readFileSync(abs, "utf8").split(/\r?\n/).map(ci);
      const ok = q.keywords.some((kw) =>
        rawLines.some((ln) => ln.includes(ci(kw)) && ln.includes(sub))
      );
      if (!ok) {
        problems.push(`[${q.id}] literal: no single raw line holds both a keyword and "${q.expect_substring}" (baseline would miss)`);
      }
      if (q.answer_format === "pdf" || q.answer_format === "docx") {
        problems.push(`[${q.id}] literal should live in html/md/txt, got ${q.answer_format}`);
      }
    } else {
      problems.push(`[${q.id}] unknown match_type ${q.match_type}`);
    }
  }

  if (problems.length) {
    throw new Error("Ground-truth verification FAILED:\n  - " + problems.join("\n  - "));
  }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------
function countFormats() {
  const counts = {};
  const walk = (dir) => {
    for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
      const p = path.join(dir, e.name);
      if (e.isDirectory()) walk(p);
      else {
        const ext = path.extname(e.name).slice(1).toLowerCase();
        counts[ext] = (counts[ext] || 0) + 1;
      }
    }
  };
  walk(CORPUS);
  return counts;
}

function main() {
  console.log(`[gen-corpus] build dir: ${BUILD}`);
  build();
  for (const d of LARGE_DOCS) {
    const bytes = fs.statSync(path.join(CORPUS, d.path)).size;
    console.log(`[gen-corpus] large ${d.format.toUpperCase().padEnd(4)} ${d.path} = ${bytes} B (${(bytes / 1024).toFixed(1)} KB)`);
  }
  console.log("[gen-corpus] corpus written, verifying ground truth (live extraction)...");
  verify();

  const formats = countFormats();
  const total = Object.values(formats).reduce((a, b) => a + b, 0);
  const byType = QUERIES.reduce((m, q) => ((m[q.match_type] = (m[q.match_type] || 0) + 1), m), {});

  // Sizes of the large context-efficiency documents (bytes on disk).
  const largeDocs = LARGE_DOCS.map((d) => ({
    path: d.path,
    format: d.format,
    bytes: fs.statSync(path.join(CORPUS, d.path)).size,
  }));

  const schema = {
    corpus_dir: CORPUS,
    queries_path: QUERIES_JSON,
    total_docs: total,
    formats,
    total_queries: QUERIES.length,
    binary_only: byType.binary_only || 0,
    large_literal: byType.large_literal || 0,
    robustness: byType.robustness || 0,
    literal: byType.literal || 0,
    large_docs: largeDocs,
    min_large_bytes: MIN_LARGE_BYTES,
    verified_extractable: true,
  };
  console.log("[gen-corpus] VERIFICATION PASSED.");
  console.log("CORPUS_SCHEMA=" + JSON.stringify(schema, null, 2));

  // best-effort cleanup of the scratch build dir
  try {
    fs.rmSync(BUILD, { recursive: true, force: true });
  } catch {}
}

main();
