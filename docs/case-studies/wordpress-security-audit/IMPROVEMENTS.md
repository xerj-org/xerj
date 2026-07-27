# Improvement ideas — where this goes next

The audit is honest about being a *first cut*. Everything below is a concrete,
scoped improvement, ordered by leverage. Each is something a contributor can pick
up; several were surfaced by the audit itself.

## A. Engine (XERJ) — make the structured index trustworthy

These are the highest leverage: they turn XERJ from "sometimes worse than grep for
reachability" into "strictly better — structured *and* complete."

1. **Multi-valued keyword `term`** *(bug the audit found; PR in progress).* A
   keyword array matches only element `[0]` on the `term` path. Fix: make
   `KeywordColumn` multi-valued (or route array fields to the source scan on every
   fast path). Restores reverse call-graph reachability. → `../../research/xerj-keyword-array-term-fix.md`
2. **Boolean `term` coercion.** `{term:{f:true}}` (JSON bool) returns 0;
   `"true"` works. Coerce JSON booleans to the stored form. Small, self-contained.
3. **Code-aware text analyzer.** The default analyzer splits identifiers on `_`
   (`esc_like` → `esc`+`like`) and drops `->`, so code search is imprecise. A
   `code` analyzer that keeps `_`, `.`, `->` as token characters makes identifiers
   searchable whole — a big precision win for the security use-case.

## B. Substrate / extractor — precision and recall

The audit's false positives all trace to a few known modeling gaps:

4. **Receiver-typed sinks.** `->query()` is dangerous on `$wpdb`, safe on
   `WP_Query`/`WP_User_Query`/`DOMXPath`. Resolve the receiver's type before
   calling a method a SQL sink. Removes essentially all SQLi false positives.
5. **Constant-path `include`/`require` are not LFI.** `require ABSPATH . '...'`
   flagged as LFI. Treat literal/const-rooted paths as safe.
6. **Self-scoped writes.** `update_user_meta(get_current_user_id(), …)` needs no
   capability — recognize the current-user object and stop flagging it.
7. **Polymorphic & delegated authorization.** Caps hide behind
   `$table->ajax_user_can()` and `$this->check_update_permission($obj)`. Follow
   these edges (class-qualified) so authz-presence isn't a false gap.
8. **Class-qualified call resolution.** Bare method names collide across classes
   (40+ controllers define `get_item_permissions_check`). Resolve `$this->m()` to
   the same class/file. *(The audit hit this as an OOP substrate bug.)*
9. **File-scope handlers.** `wp-admin/*.php` handle requests in a top-level
   `switch($action)` outside any function; index those case blocks (with guards
   gathered from the enclosing scope). *(The audit's biggest recall gap.)*
10. **Deeper taint.** Model flow through arrays/`$GLOBALS`, aliasing, and
    variable-variables — today taint follows direct call edges only.

## C. Detector coverage — more classes, more frameworks

11. **Completeness detectors for every validator class**, generalizing
    `wp_ssrf_ranges.py`: SSRF ranges, redirect allow-lists, upload MIME/extension
    lists, CORS origins, `wp_kses` tag/attr sets. The bug class is *"a defense
    that's present but incomplete"* — compile each as a queryable coverage check.
12. **Deserialization sinks** — untrusted `unserialize`/`pickle.loads`/
    `yaml.load`/`ObjectInputStream` reaching a source, with the HMAC-gate
    recognized as a defense.
13. **Path traversal** — `include`/file-open with a request-derived path lacking a
    normalization/allow-list.
14. **More frameworks** — Laravel, Symfony, Django, Rails, Express/Next, Spring.
    The taint model is data; add framework fact-lists, reuse the engine.

## D. Product / workflow — where XERJ should meet the auditor

15. **AST-aware `autoindex`.** Ship the function-level extractor *inside*
    `xerj autoindex ./code` so the graph-ready index is one command, not a
    separate script. (The research doc's original "next step.")
16. **Findings as a first-class index.** Store confirmed findings with a
    severity/route schema so the agent loop is "query findings → pull path code →
    confirm," and re-audits diff against the last run.
17. **MCP-native auditing.** Expose the audit queries via `xerj-mcp` so an AI
    agent runs the whole playbook through native tools — no hand-written curl.
18. **CI mode.** Run the fingerprint queries on every PR; fail on a *new*
    escaper-last violation, incomplete validator, or unguarded object-scoped op.
    The fingerprints make this O(diff), not O(codebase).

## E. Rigor — trust the automation less, verify more

19. **Adversarial verification pass** — for each surviving finding, an independent
    agent whose only job is to refute it; keep only what survives.
20. **Sandbox execution harness** — where the audit ported `wp_http_validate_url`
    to Python by hand, wire a real PHP/Node/Python sandbox so candidate flows are
    *executed* automatically, not transcribed.

---

## The one-paragraph close

This case study started as "can an AI audit real WordPress cheaply?" and ended as
something more useful: a **method** — index once, interrogate, read only what the
graph points to, verify by execution, and compile every invariant you learn into a
fingerprint the next audit queries for free. It found one real (known-class) SSRF
gap, verified core is otherwise hardened without manufacturing a single finding,
and — most tellingly — surfaced and fixed a bug in XERJ itself. The token
economics (~199× vs reading everything, ~2,700× on a compiled invariant) are real,
but the durable output is the growing library of detectors and the honesty
discipline. Point it at your own codebase, add your framework's fact-lists, and
improve the loop — that is the invitation.
