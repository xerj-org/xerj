# The WordPress authentication & authorization story (all entry surfaces)

WordPress accreted **four** request-entry surfaces over 20 years, and an auth audit
has to cover all of them — a check that's correct for AJAX means nothing if the
same operation is reachable unguarded over REST or XML-RPC. This is the
consolidated story: the model, every surface, the specific failure classes
(missing / insufficient / broken-`==` / race), and the honest verdict — with the
XERJ-vs-plain-tooling cost measured.

## The model (three independent things)
Every state change needs all three; conflating them is the #1 WP auth bug:
1. **Authentication** — *who* (auth cookie / `wp_authenticate`).
2. **Authorization** — *allowed?* — a **capability bound to the object**:
   `current_user_can('edit_post', $post->ID)`, not a generic `current_user_can('edit_posts')`.
3. **Intent** — *did they mean it?* — a nonce / CSRF token. **`check_ajax_referer` verifies intent, NOT authorization** — a Subscriber holds valid nonces.

## The four surfaces, audited (from XERJ facts)

| surface | entry points | auth posture | verdict |
|---|--:|---|---|
| **AJAX** (`wp_ajax_*`) | 97 | object-scoped cap + nonce; a few self-scoped (prefs) need no cap | **0 IDOR** |
| **REST** (`register_rest_route` → `*_permissions_check`) | 107 checks | object-scoped meta-cap one hop into `check_*_permission($obj)`; `create_*` uses generic caps correctly (no object yet) | **0 IDOR** |
| **admin-post / direct `wp-admin`** | 41 file-scope handlers | object-scoped cap + `check_admin_referer`, guards in shared preamble/fall-through | object-scoped |
| **XML-RPC** (`class-wp-xmlrpc-server`) | 106 methods | 66 call `login()`, 68 cap-check | see below |

### XML-RPC specifics (the oldest, riskiest surface)
- **Unauthenticated methods** are the known set and intentional: `pingback.*`
  (→ **SSRF/DoS**, and it hits Finding 1's `169.254` gap via `wp_safe_remote_request`),
  `demo.sayHello`/`addTwoNumbers` (harmless), `mt.supportedMethods`/`supportedTextFilters`
  (public info). `blogger.getTemplate`/`setTemplate` are **disabled** (`IXR_Error 403`).
- **The structural weakness — brute-force amplification:** every authenticated
  method calls `$this->login($user,$pass)` (username/password, no per-call rate
  limit or nonce), and **`system.multicall` batches many calls into one HTTP
  request** → hundreds of credential guesses per request. This is by-design and the
  reason hardened sites disable XML-RPC. Not a code bug — an architectural exposure.

## The specific failure classes (what we looked for)

| class | method | result in core |
|---|---|---|
| **Missing** permission check | every state-changing entry point, all surfaces | none — all gated |
| **Insufficient** (generic cap on a specific object → IDOR) | `has_obj_cap` vs `has_generic_cap`, interprocedural + check-vs-use | none — object-scoped meta-caps; the one risky shape (`revisions`) has an explicit `parent_id_mismatch` guard |
| **Broken `==`** (loose compare / magic-hash in auth) | `wppatterns` loose-eq/timing-compare ∩ auth files | none — the loose-`==` hits in auth files are Akismet **status-string** compares (`'spam' == $status`), benign; core secrets use `wp_check_password` / `hash_equals` |
| **Race / TOCTOU** in auth | check-then-use in privileged paths | none found in core; note: WP nonces are **reusable within their 12–24h window** (documented property, not a race), and object-scoped `current_user_can` is synchronous |
| **`map_meta_cap` fail-open** | the authz engine | fails **closed** (`do_not_allow` on missing object/arg) |

## Honest verdict
Across all four surfaces, **WordPress core's authorization is consistently
object-scoped and fail-closed; no missing, insufficient, broken-`==`, or race
auth flaw was found.** The real exposures are *architectural, not code bugs*:
XML-RPC brute-force amplification and pingback SSRF (both mitigated by disabling
XML-RPC), and the SSRF range gap (Finding 1). The lesson the census makes provable:
**the same operation must be checked identically on every surface** — that's what
plugins get wrong, and what this multi-surface sweep is built to catch.

## XERJ vs plain tooling (measured, this audit)
| approach | tokens | quality |
|---|--:|---|
| grep + read the 50 handler files (AJAX + REST + XML-RPC + admin) | **~335,500** | must hand-trace each cap check across files; exceeds a context window |
| **XERJ** — auth-posture facts for all 310 entry points (`has_obj_cap`/`has_nonce`/`caps`) | **~6,600** | structured posture per entry point, one query per surface, reusable |

**~51×** fewer tokens, and it answers the cross-surface question directly (*"which
entry points change state with only a generic cap?"*) — which grep can't express.
The full authz reasoning behind each verdict is in
[`wordpress-authz-agentic-audit.md`](../../research/wordpress-authz-agentic-audit.md).
