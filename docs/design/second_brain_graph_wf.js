export const meta = {
  name: 'second-brain-graph-ux',
  description: 'Research and rebuild the second-brain UX as an all-in-one visual knowledge graph with a helicopter default view, make the dashboard appear only after a brain exists, prove it on the whole repo (code+PDF+docs), and add a screencast scene',
  phases: [
    { title: 'Research', detail: 'Obsidian + the hairball literature + the existing ledger rationale' },
    { title: 'Design', detail: 'one decision set: helicopter view, drill-down, stats, cross-type relations' },
    { title: 'Build', detail: 'graph view, LOD/clustering, stats panel, conditional dashboard appearance' },
    { title: 'Corpus', detail: 'index the whole repo — 430 PDFs, 208 crates-worth of Rust, 6.6k markdown' },
    { title: 'Prove', detail: 'scale + drill-down + cross-type relations verified on that corpus' },
    { title: 'Screencast', detail: 'add a second-brain scene to AISE without breaking the existing demo' },
  ],
}

const REPO = '/home/claude/ai/xerj'
const AISE = '/home/claude/ai/aise'
const SPEC = `${REPO}/docs/design/SECOND_BRAIN_SPEC.md`
const SCRATCH = '/tmp/claude-1001/-home-claude-ai-xerj/e36b8c4c-3a94-4a76-80f1-2119380319c6/scratchpad'

const GROUND = `
## The mission

Make XERJ's second brain a knowledge-graph UX people LOVE: an all-in-one visual graph
that is never overwhelming no matter how big the corpus, proven on this repo's own
heterogeneous content (code + PDFs + docs), and captured as a screencast.

## Where things stand (verified today — do not re-derive)

- Branch \`feat/second-brain\` in the MAIN repo at ${REPO} (already checked out). The
  feature WORKS end-to-end and is spec-conformant: \`xerj brain <folder>\` indexes a folder
  into a bi-temporal, evidence-carrying link graph; HTTP API is
  \`POST/DELETE /_graph/{brain}/link\`, \`GET /_graph/{brain}/ego\`, \`GET /_graph/{brain}/overview\`.
  Contract: ${SPEC} (normative). Live-verified: 10 notes → 25 links in 0.1s; the repo's
  \`docs/\` → 364 links in 0.7s.
- **The current UX is a text LEDGER, not a node-link graph — deliberately.**
  \`xerj-ux/src/ux/ego-ledger.js\` (~1,100 lines) opens with: *"The graph view is a LEDGER,
  not a hairball. A global force-directed [layout is rejected]"*. READ THAT FILE AND ITS
  RATIONALE FIRST. It is well-reasoned and its evidence-first design (every link shows the
  quote that created it) is the product's best idea. You are not deleting it — you are
  ADDING the visual graph the user asked for, and reconciling it with that rationale.
- Panels/dashboard: \`xerj-ux/src/dashboards/second-brain.js\`, data layer
  \`xerj-ux/src/data/second-brain-api.js\`, registry \`xerj-ux/src/dashboards/registry.js\`,
  console seed \`engine/crates/xerj-console-api/src/seed.rs\`.

## What the user asked for, precisely (these are the acceptance criteria)

1. **An all-in-one visual knowledge-graph view** — a real graph, plus statistics: which
   data was used / read / searched **and why**, and relations between objects across
   different files and file types.
2. **SUCCESS CRITERION (the hard one):** the graph must NOT look overcomplicated or
   oversized *regardless of the size, depth and complexity of the data* — because the
   default is an **auto "helicopter" view** that always auto-selects the largest
   topics/connections at the start — **and** drill-down to the deepest level exists and
   works **smoothly**.
3. **The dashboard must appear only AFTER a brain has been indexed.** Today it is
   hardcoded into the registry and console seed, so a user who never ran \`xerj brain\`
   still sees an empty "Second Brain" dashboard — confusing. Make its presence conditional
   on at least one brain existing.
4. **Really test on a large, heterogeneous corpus** — this repo has **430 PDFs**, 208
   Rust files, ~6,662 markdown files, 75 JS files, plus gtm/ marketing papers. The point is
   to see how a PDF/marketing paper actually relates to crates and functions in the code.
5. **Screencast**: add a second-brain scene to the AISE demo engine. **DO NOT break or
   remove the existing demo scenes** — add to them.

## Binding product rules (violating these is a defect)

- **LANGUAGE RULE:** every human-visible surface says "link / believed since / retired /
  what taught this". NEVER schema vocabulary (\`edge\`, \`src\`, \`dst\`, \`valid_at\`, \`as_of\`)
  in UI text.
- **HONESTY RULE:** XERJ is **not a graph database** and must never be presented as one.
  Traversal is capped at 2 hops per call (\`hops>2\` returns a 400 saying exactly this);
  deeper drill-down must be built by iterating from the response's reachable set, not by
  pretending the engine does variable-depth traversal.
- **Detectors are structural/lexical** (wiki-links, markdown links, hrefs, folder
  adjacency, section sequence) — there is no semantic understanding and no LLM. The
  embedder is \`lexical-feature-hash\`. Any "relates to" claim in the UI must be truthful
  about which detector produced it.
- Keep evidence-first: a link's quote is the product's best idea; the visual graph must
  not lose it.

## Build & run discipline (hard rules)

- Cargo workspace ${REPO}/engine. Scoped builds ONLY: \`cargo build --release -j 32 -p <crate>\`.
  NEVER workspace-wide, NEVER \`cargo clean\`. fmt + clippy -D warnings clean on touched crates.
- JS: no build step — plain ES modules, match the house style of the existing dashboards.
  The console CSP/offline context blocks CDNs: **no external libraries** (no d3, no cytoscape).
  Write the graph yourself; prefer **Canvas 2D** for node/edge rendering at scale.
- Server boots: foreground boots are REAPED (exit 144). Use the Bash tool's
  \`run_in_background: true\`, then poll
  \`until curl -s -o /dev/null -w '%{http_code}' localhost:PORT/ | grep -q 200; do sleep 2; done\`.
- **WebAuthn gotcha (cost an hour today):** the console's relying party is hardcoded to
  \`http://localhost:9200\` (\`xerj-console-api/src/state.rs\` RpConfig default). Passkey
  enrollment ONLY works when the console is served on **port 9200** and browsed via
  **localhost** (not 127.0.0.1 — Chrome rejects raw IPs as an RP). Use 9200 for any
  browser work; use 9330-9345 for headless API-only servers.
- Headless browser IS available: \`puppeteer\` resolves from ${REPO}/node_modules (run node
  from ${REPO}), Chrome at /usr/bin/google-chrome. Enrollment works via a CDP virtual
  authenticator: \`WebAuthn.enable\` + \`WebAuthn.addVirtualAuthenticator({protocol:'ctap2',
  transport:'internal', hasResidentKey:true, hasUserVerification:true, isUserVerified:true,
  automaticPresenceSimulation:true})\`, then claim via the setup magic-link the server prints
  at first boot. A working reference script is ${REPO}/sb_capture.mjs.
- \`xerj brain\` gotcha: a stale resume journal in \`~/.xerj/autoindex\` makes re-runs skip
  detection. Use \`--fresh\` (or clear it) for clean runs; note that \`--fresh\` rebuilds
  the plan in place and never removes documents for notes you deleted.
- Commits authored \`xerj-org <git@xerj.org>\`, NO Claude co-author trailer, never mention
  ctrl-frk. Do NOT push. Commit on \`feat/second-brain\`.

## Honesty rules (a public demo comes out of this)

- Every number you publish must come from something you RAN. Never estimate silently.
- If the graph IS a hairball at 430-PDF scale, say so and fix it — do not ship a
  screenshot of the one corpus where it looked fine.
- "This does not work yet" is a valuable result. Overclaiming is the only failure mode
  that matters.
`

phase('Research')

const RESEARCH = [
  {
    key: 'obsidian',
    title: 'Obsidian graph view (and its peers) — what it does, and where it fails',
    brief: `Research Obsidian's graph view in depth using WebSearch/WebFetch (current facts,
not memory): the global graph vs the LOCAL graph, depth setting, filters/search-in-graph,
groups + colour rules, forces (centre/repel/link), "orphans", and the Canvas feature. Then
the honest part: the widely-documented criticism that the global graph is a pretty hairball
nobody uses. Find what people ACTUALLY find useful (local graph at depth 1-2, filtered
subgraphs, colour groups) and what they abandon.
Also survey the peers for scale techniques: Logseq, Roam, Kumu, Gephi, and the research
tradition — community detection / clustering for overview, level-of-detail, semantic zoom,
focus+context, degree-of-interest filtering, edge bundling, "overview first, zoom and
filter, details on demand" (Shneiderman).
Deliver: a concrete list of what to copy, what to avoid, and specifically HOW tools keep a
graph readable as N grows into the thousands. Cite URLs.`,
  },
  {
    key: 'helicopter',
    title: 'The helicopter view: how to auto-summarise a large graph so it is never a hairball',
    brief: `This is the user's hard success criterion, so treat it as an engineering problem.
Research and then specify concrete algorithms that run in plain JS on Canvas with no
libraries, over a graph of up to ~50k links:
- **Clustering / community detection** cheap enough for the browser (label propagation,
  Louvain-lite, or connected-components + degree ranking). How do we pick "the largest
  topics" automatically?
- **Level of detail / semantic zoom**: what is drawn at helicopter altitude (clusters as
  super-nodes sized by membership) vs mid vs leaf level, and how the transition stays smooth.
- **Layout without a global force simulation**: deterministic seeded placement, radial/ring
  layouts per cluster, or a coarse force run on the CLUSTER graph only (a few dozen nodes)
  rather than all N. Remember the existing ledger explicitly rejected a global force sim —
  respect the reason (jitter, non-determinism, hairball) while still giving a graph.
- **Drill-down**: how expanding a cluster stays smooth (animated, bounded work per frame),
  and how it composes with the engine's 2-hop cap (you iterate ego calls from the reachable
  set — you must NOT claim variable-depth traversal).
- **Budgets**: hard caps on nodes/edges drawn per frame, and what the UI honestly says when
  it is showing a summary rather than everything.
Deliver precise algorithm choices with complexity, the data the API must supply, and the
draw budget. Be concrete enough to implement.`,
  },
  {
    key: 'ledger-and-api',
    title: 'What the existing UX and API can already give us',
    brief: `Read the real code, not docs: \`xerj-ux/src/ux/ego-ledger.js\` (all of it — the
rationale comment at the top is important), \`xerj-ux/src/dashboards/second-brain.js\`,
\`xerj-ux/src/data/second-brain-api.js\`, \`xerj-ux/src/ux/panel-render.js\` +
\`charts*.js\` (the house viz primitives you must match), \`xerj-ux/assets/base.css\` (the
design tokens/house style), and \`xerj-ux/src/dashboards/registry.js\`.
Then the server side: \`engine/crates/xerj-api/src/graph_api.rs\` — exactly what
\`/ego\` and \`/overview\` return today (fields, caps, \`not_shown\` accounting), and
\`engine/crates/xerj-console-api/src/seed.rs\` for how dashboards are seeded.
Answer: (a) what does the graph view need that the API does NOT return today (e.g. a
whole-brain node+edge dump for layout, cluster hints, per-node degree, file-type facets)?
Specify the exact new/extended endpoint shape needed, honouring the spec's conventions.
(b) How is the dashboard currently registered, and what is the cleanest way to make it
appear only when ≥1 brain exists — list the exact files/functions to change.
(c) Which house primitives should the graph reuse so it looks native.`,
  },
  {
    key: 'stats-and-crosstype',
    title: 'The statistics story and cross-file-type relations',
    brief: `The user wants "statistics which data used/read/search and why" and "relations
between objects from different files" — including how a PDF or marketing paper relates to
crates and functions in code.
Investigate what is TRULY available: read \`engine/crates/xerj-autoindex/src/detect/\`
(all detectors: wikilink, mdlink, href, samedir, sequence, e2e) and the extractors
(\`extract/\` — including the PDF path). Determine honestly which cross-file-type links
this system can actually produce over this repo: can a PDF in \`landing/resources/\` or
\`gtm/\` ever link to a Rust crate? Through what mechanism (an href? a filename mention? a
same-directory rule? nothing at all)?
If the answer is "structurally, almost never", say so plainly and propose what WOULD create
honest cross-type relations (e.g. a detector that links a document to a code file when it
cites the path/crate name — and what its evidence quote would be). Any new detector must
follow the spec's determinism + evidence rules (${SPEC} §6).
Also specify the statistics panel: what counts are real and available (per detector, per
file type, per node degree, ingest/read counts if the engine exposes them), and what is NOT
available and must therefore not be shown. "Which data was searched and why" must map to
something real — say what.`,
  },
]

const research = await parallel(RESEARCH.map((r) => () =>
  agent(
`RESEARCH — ${r.title}

${GROUND}

## Your assignment
${r.brief}

Use WebSearch/WebFetch for anything about the outside world; read the REAL code in ${REPO}
for anything about XERJ. Be concrete and opinionated — a list of vague considerations is a
failed result. Return a thorough report.`,
    { label: `research:${r.key}`, phase: 'Research', model: 'fable', effort: 'high' }
  )
))

phase('Design')

const design = await agent(
`DESIGN — turn the research into ONE decision set an implementer can execute.

${GROUND}

## Research: Obsidian + peers
${String(research[0] || '').slice(0, 15000)}

## Research: helicopter view algorithms
${String(research[1] || '').slice(0, 15000)}

## Research: existing UX + API
${String(research[2] || '').slice(0, 15000)}

## Research: statistics + cross-type relations
${String(research[3] || '').slice(0, 13000)}

Produce THE DESIGN, as decisions not options:

1. **The view.** The all-in-one second-brain screen: what is on it, in what order, and what
   the visual graph looks like at helicopter altitude vs drilled in. Reconcile explicitly
   with the existing ledger's anti-hairball rationale — say what the graph adds, what the
   ledger keeps, and how a user moves between them. The evidence quote must survive.
2. **The helicopter algorithm.** Exact clustering + layout + LOD choices, draw budgets, and
   the interaction model for drill-down (including how it composes with the 2-hop API cap
   without lying about traversal depth). State the target: readable at 25 links AND at
   50,000 links.
3. **The API delta.** Exact new/extended endpoint(s) with request/response shapes, honouring
   ${SPEC} conventions (contract string, \`not_shown\` accounting, caps). Keep it minimal.
4. **Conditional appearance.** Exact mechanism + files so the dashboard only shows when ≥1
   brain exists, including what a user with no brain sees instead (an honest empty/CTA state
   somewhere sensible, or nothing at all — decide).
5. **Statistics panel.** Only real, available numbers — list them, and list what you refuse
   to show because the data does not exist.
6. **Cross-type relations.** The honest answer for PDF↔code, and if a new detector is
   warranted, its exact spec-conformant definition (tag, weight, confidence, evidence rule,
   determinism).
7. **Work breakdown** into 4-6 parallel implementation tasks with disjoint file sets and
   crisp acceptance criteria.

Return a detailed written document — it IS the implementation brief.`,
  { label: 'design', phase: 'Design', model: 'fable', effort: 'high' }
)

phase('Build')

const TASKS = [
  { key: 'graph-view', title: 'The Canvas knowledge-graph view + helicopter LOD + drill-down',
    files: 'xerj-ux/src/ux/ (new graph module), xerj-ux/assets/base.css' },
  { key: 'api-delta', title: 'The API delta the graph needs (whole-brain summary / cluster feed)',
    files: 'engine/crates/xerj-api/src/graph_api.rs, engine/crates/xerj-engine/src/graph.rs' },
  { key: 'dashboard-stats', title: 'Dashboard composition + the statistics panel + ledger integration',
    files: 'xerj-ux/src/dashboards/second-brain.js, xerj-ux/src/data/second-brain-api.js, xerj-ux/src/ux/ego-ledger.js' },
  { key: 'conditional', title: 'Conditional dashboard appearance (only after a brain exists)',
    files: 'xerj-ux/src/dashboards/registry.js, xerj-ux/src/app.js, engine/crates/xerj-console-api/src/seed.rs' },
  { key: 'crosstype', title: 'Cross-file-type relations (detector work, if the design calls for it)',
    files: 'engine/crates/xerj-autoindex/src/detect/, docs/design/SECOND_BRAIN_SPEC.md amendment' },
]

const built = await pipeline(
  TASKS,
  (t) => agent(
`BUILD — ${t.title}

${GROUND}

## THE DESIGN (your brief — follow it)
${String(design).slice(0, 24000)}

## Your slice
${t.title}. Primary files: ${t.files}. Other agents work the other slices IN PARALLEL in
the same tree — stay in your file set; if you must touch a shared file, make the smallest
possible edit.

Rules:
- No external JS libraries (CSP + offline). Canvas 2D for the graph. Plain ES modules.
- Match the house style exactly (base.css tokens, the existing dashboards' idiom).
- Rust: scoped release builds, fmt + clippy -D warnings clean, extend the tests.
- Honour the LANGUAGE and HONESTY rules in every string you write.
- Do NOT commit. Do NOT push.
- VERIFY YOUR OWN WORK: build it; where it is JS, exercise it headlessly in node or in
  puppeteer against a live server (port 9200 for browser work — see the WebAuthn note).
Return: files changed, what you built, your verification evidence, judgement calls, and
anything you could not do.`,
    { label: `build:${t.key}`, phase: 'Build', model: 'fable', effort: 'high' }
  ),
  (result, t) => agent(
`REVIEW the slice "${t.title}" adversarially, then FIX what is wrong.

${GROUND}

## What the implementer claims
${String(result).slice(0, 12000)}

1. Verify every claim — read the real diff (\`git diff\` in ${REPO}), build it, run it.
   A claim that is not true is your finding.
2. Check the LANGUAGE rule (no schema vocabulary in UI text) and the HONESTY rule (never
   "graph database"; no fake traversal depth) on every user-visible string.
3. Check it against ${SPEC} where relevant.
4. Fix defects minimally. Do not redesign.
Return what you verified, what was wrong, what you fixed, and the final state.`,
    { label: `review:${t.key}`, phase: 'Build', model: 'fable', effort: 'high' }
  )
)

phase('Corpus')

const corpus = await agent(
`INDEX THE WHOLE REPO — the large heterogeneous corpus test.

${GROUND}

## What was built
${JSON.stringify(built).slice(0, 12000)}

This repo contains **430 PDFs** (landing/resources, gtm/, gtm/competitors/**), **208 Rust
files**, **~6,662 markdown files**, 75 JS files. That is the corpus.

Do this:
1. Build the chain scoped and clean first.
2. Boot a server on **port 9200** (browser work later needs that port), fresh data dir under
   ${SCRATCH}, and clear \`~/.xerj/autoindex\` so nothing resumes stale.
3. Run \`xerj brain\` over the repo. Start with a large but sane scope and grow — e.g.
   \`docs/\` + \`gtm/\` + \`landing/\` + \`engine/crates/\`. Capture the REAL terminal output and
   timings at each scale. If it is slow or falls over, that is a finding — report it with
   numbers, then work out whether it is a real limit or a config issue.
4. Report the true shape of the result: how many files, how many links, per-detector
   breakdown, per-file-type breakdown, the biggest hubs, and how many links are AUTHORED vs
   STRUCTURAL.
5. **The cross-type question:** find real examples of a PDF or marketing paper related to a
   crate/code file. Show the actual link with its evidence. If cross-type links essentially
   do not form, SAY SO with the numbers — that is the honest answer and the design should
   have predicted it.
6. Leave the server RUNNING on 9200 with the corpus indexed — the next phase needs it. State
   the brain name(s) clearly.

Return: exact commands, real output, the measured numbers, the cross-type findings (with
evidence), and any failure or limit you hit.`,
  { label: 'index-large-corpus', phase: 'Corpus', model: 'fable', effort: 'high' }
)

phase('Prove')

const proof = await agent(
`PROVE THE SUCCESS CRITERION — visually, at scale, headlessly.

${GROUND}

## The design
${String(design).slice(0, 8000)}

## What was built
${JSON.stringify(built).slice(0, 8000)}

## The indexed corpus (server should be live on 9200)
${String(corpus).slice(0, 10000)}

The user's criterion: **the graph must not look overcomplicated or oversized regardless of
size/depth/complexity — the helicopter default auto-selects the largest topics — and
drill-down to the deepest level works smoothly.**

Do this:
1. Drive the REAL console in puppeteer (port 9200, virtual WebAuthn authenticator — see
   ${REPO}/sb_capture.mjs for a working claim flow). Screenshot the second-brain view on the
   LARGE corpus at: (a) helicopter default, (b) one drill-down step, (c) deepest drill-down,
   (d) the statistics panel, (e) an evidence quote open.
   Save PNGs under ${SCRATCH}/graphshots/.
2. **Judge it honestly against the criterion.** Is the default view readable, or is it a
   hairball? Count the drawn nodes/edges at each level. Measure frame/interaction time if you
   can. If it fails the criterion, FIX IT and re-shoot — that is the job, not a caveat.
3. Verify the conditional appearance: with a brain present the dashboard shows; point the
   console at a fresh empty server (different port/data dir) and confirm it does NOT appear
   (or shows the designed empty state). Screenshot both.
4. Verify the honesty rules survived on real rendered text: no schema vocabulary, no
   "graph database", no fake traversal-depth claim.
Return: the screenshot paths, the counts/timings, a blunt verdict on the success criterion,
and what you fixed to meet it.`,
  { label: 'prove-at-scale', phase: 'Prove', model: 'fable', effort: 'high' }
)

phase('Screencast')

const screencast = await agent(
`SCREENCAST — add a second-brain scene to the AISE demo engine.

${GROUND}

## The proven UX
${String(proof).slice(0, 10000)}

AISE lives at ${AISE} (no git — be careful, there is no undo). It records the Xerj demo:
puppeteer drives scenes, \`src/lib/recorder.js\` captures CDP frames into a constant-fps mp4
so video-time == wall-clock, and \`src/lib/voice.js\` (edge-tts) muxes narration at each
tooltip's \`start_ms\`. Existing scenes: \`src/scenes/01-boot.js\`, \`02-show-data.js\`,
\`03-bulk-ingest.js\`, \`04-verify.js\`, \`05-open-xerj-console.js\`, \`06-walk-spa.js\`,
wired in \`src/orchestrator.js\` (~line 200).

**DO NOT modify or remove the existing scenes or the existing demo flow.** ADD a new scene
(e.g. \`07-second-brain.js\`) and wire it in after the console walk, so the existing demo
still runs exactly as before.

The user's standing requirements for these videos (binding):
- **No usernames or home paths on screen** — everything under \`/tmp/xerj\`, never \`/home/<user>\`.
- Narration never speaks the eyebrow label ("SCENE 7"), never spells out code punctuation
  (say "POST bulk", not "slash underscore bulk"), no spaced-thousands (voice code-switches).
- **No large silent gaps** — narration plays OVER the action, not after it.
- Interactions must show the product WORKING, not just navigation.
- Console/WebAuthn must be on **:9200** (rp_origin).
- Pre-warm the voice cache before recording, else edge-tts latency stalls the run.

The scene should tell the second-brain story: one command over a real folder → the graph
appears at helicopter altitude → drill into a cluster → open a link's evidence quote →
(if it works) the retire/replay beat.

Running the recorder needs Chrome + port binds: use the Bash tool's \`run_in_background\`
(a plain foreground run dies exit 144). A full render takes ~5-8 minutes.

Do this:
1. Write the scene, following the house patterns of the existing scenes exactly (read
   \`06-walk-spa.js\` and \`src/lib/stage.js\` first).
2. Wire it into the orchestrator additively.
3. Stage whatever the scene needs (a notes/corpus folder under /tmp/xerj, the built binary).
4. **Try to actually render an mp4.** If the full pipeline runs, report the output path and
   verify sync (there is a \`review.py\`). If the environment blocks a full render, say so
   honestly and leave a single documented command that produces it.
Return: files added/changed (and confirmation the existing scenes are untouched), the mp4
path if produced, and anything that blocked you.`,
  { label: 'screencast', phase: 'Screencast', model: 'fable', effort: 'high' }
)

return {
  design_summary: String(design).slice(0, 3000),
  built: TASKS.map((t) => t.key),
  corpus: String(corpus).slice(0, 6000),
  proof: String(proof).slice(0, 8000),
  screencast: String(screencast).slice(0, 5000),
}
