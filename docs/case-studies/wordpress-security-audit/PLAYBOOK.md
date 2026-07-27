# Playbook: run this security audit yourself, with an AI + XERJ

This is the step-by-step guide to reproduce the audit **the way the agent did it**
— not just running the scripts, but driving an AI (Claude, or any model with tool
access) to use XERJ as its reasoning substrate. Copy the prompts, point them at
your own codebase, improve them.

The method in one line: **index the code once, then make the AI *interrogate* the
index — query → read only what it points to → reason → next query — instead of
loading the whole codebase.**

---

## Step 0 — Setup (≈2 minutes)

```bash
pip install -r ../../examples/ast-vuln-graph/requirements.txt
git clone --depth 1 https://github.com/WordPress/WordPress.git ./wordpress
xerj --insecure --data-dir ./wpdata &          # ES-compatible wire on :9200
cd ../../examples/ast-vuln-graph
python3 wp_audit_index.py   ./wordpress        # wpaudit + wphooks
python3 wp_authz_graph.py   ./wordpress        # wpauthz
python3 wp_compose_index.py ./wordpress        # wpcompose
```

You now have a searchable **security substrate**: one document per function
(code + taint facts), one per hook, plus fingerprint indices. This is what the AI
thinks against.

---

## Step 1 — The master prompt

Give your AI agent tool access to shell/curl against `http://127.0.0.1:9200`, then
paste this. It encodes the whole method, including the honesty rules that make the
result trustworthy.

> **You are a security code auditor. XERJ (Elasticsearch-compatible, on
> `127.0.0.1:9200`) is your second brain: it holds every function of the target
> codebase as a searchable document with taint facts (`sources`, `sinks`,
> `sanit`, `calls`), plus a hook index (`wphooks`) and fingerprint indices
> (`wpauthz`, `wpcompose`). Do NOT load whole files. Instead: form a hypothesis,
> query XERJ to find the relevant functions, pull ONLY those bodies, read them,
> reason, and query again.**
>
> **Rules that make your findings trustworthy:**
> 1. **Precision from the graph, navigation from search.** Let the taint/authz
>    facts decide what is a candidate; use full-text only to navigate. FTS over
>    raw code is noisy for security.
> 2. **Verify by reading, then by executing.** Every candidate must be confirmed
>    by reading the real implementation. For a claimed bug, reconstruct and run
>    the exact logic (port it to a runnable language if needed) before asserting
>    it.
> 3. **Trace reachability to real input.** A sink or gap only matters if
>    attacker-controlled input reaches it — find the caller chain.
> 4. **No manufactured findings.** If the code is correctly defended, say so and
>    show why. A clean negative is a valid result.
> 5. **Report substrate bugs.** If a query result is impossible (e.g. a function
>    with a call that grep finds but the index doesn't), suspect the index/engine,
>    not reality — investigate and report it.
> 6. **Cite `file:line` from pulled code for every claim.** Never reason from
>    memory of the framework.
>
> **Caveats for querying XERJ today (two known engine bugs):** `term` on a keyword
> *array* matches only the first element — for reverse call-graph reachability,
> pull `_source` and traverse the `calls` arrays in code, or use `match`. Boolean
> `term` matches as a string — use `{"term":{"has_source":"true"}}`, not `true`.
>
> **Begin by mapping the attack surface: query `wphooks` for unauthenticated entry
> points, and enumerate the framework's request handlers. Then work inward.**

---

## Step 2 — Phase prompts (what the agent does, and the queries)

Run these as follow-ups. Each is a hypothesis + the XERJ query + what to look for.

### 2a. Unauthenticated attack surface
> *"List every unauthenticated entry point and what it reaches."*
```bash
curl -s "$XERJ/wphooks/_search" -H 'Content-Type: application/json' \
  -d '{"query":{"term":{"unauth":true}},"_source":["hook","callback","file"]}'
```
Read each callback. Ask: does raw input reach a sink or dispatch to a filter?
(In WP core: 2 of 1,343 hooks; the only unauth data path is `heartbeat_nopriv_received`.)

### 2b. Injection reachable from request input
> *"Find functions that read a request source AND hit a SQL sink AND are not
> sanitized in-function; read all of them."*
```bash
curl -s "$XERJ/wpaudit/_search" -H 'Content-Type: application/json' \
  -d '{"query":{"bool":{"filter":[{"term":{"has_source":"true"}},{"term":{"sinks":"sql"}},{"term":{"sanit":"false"}}]}}}'
```
11,990 functions → a handful of candidates. Read each; check the *receiver type*
of the sink (`$wpdb->query` is raw; `WP_Query->query` is safe).

### 2c. Authorization / IDOR
> *"Are privileged operations gated by an OBJECT-SCOPED capability, or a generic
> one? The bug is a generic cap (or none) on an object named by request input."*
Read the gatekeepers first (`current_user_can`, `wp_verify_nonce`,
`check_ajax_referer`) to learn the correct pattern, then classify every handler.
`wp_authz_graph.py` and `wp_checkuse_idor.py` automate the classification.

### 2d. Completeness of a defense (the class pattern scanners miss)
> *"For each security VALIDATOR (SSRF host check, escaper, allow-list), enumerate
> what it covers and find what it MISSES."*
```bash
python3 wp_ssrf_ranges.py    # flags wp_http_validate_url missing 169.254 / metadata
python3 verify_ssrf.py       # then EXECUTE the flow to confirm
```

### 2e. Sanitizer composition (one filter breaking another)
> *"Find any function where a de-escaper runs AFTER an escaper before a sink."*
```bash
curl -s "$XERJ/wpcompose/_search" -H 'Content-Type: application/json' \
  -d '{"query":{"bool":{"filter":[{"term":{"compose_danger":"true"}},{"term":{"sink_raw":"true"}}]}},"_source":["func","file","sanitizer_seq"]}'
```
The `sanitizer_seq` fingerprint self-triages most; read the survivors.

---

## Step 3 — Point it at YOUR codebase

The taint model is **data, not code**. To audit a different stack:

1. **Add the language** — `scan_multilang.py` already covers PHP/Python/Go/Rust/
   Java via tree-sitter; add a `LANGS` row (sources / sinks / sanitizers) for
   yours.
2. **Edit the framework lists** — replace WP's `$_GET`/`$wpdb`/`esc_*`/
   `current_user_can` with your framework's request objects, DB sinks, escapers,
   and auth primitives (Django `.raw()`/`@login_required`, Express `req.query`/
   `knex.raw`, Rails `params`/`where("...#{}")`, etc.).
3. **Re-index and re-prompt** — the master prompt is framework-agnostic; only the
   fact lists change.

---

## Step 4 — Improve the loop

- Add a **completeness critic** pass: after each phase, prompt *"what did we NOT
  check — a surface, a validator, a sink class?"* and turn the answer into the
  next query.
- Add **adversarial verification**: for each surviving finding, spawn a second
  read whose only job is to *refute* it; keep it only if refutation fails.
- **Compile every new invariant into a fingerprint** (like `sanitizer_seq`), so
  the next audit queries it for ~free instead of re-reading. This is the
  compounding win.

See [`IMPROVEMENTS.md`](IMPROVEMENTS.md) for the roadmap that closes this case
study.
