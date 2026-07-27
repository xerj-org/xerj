# Thinking with XERJ: an agent reasons about WordPress authorization

This is a different exercise from [the grep comparison](wordpress-audit-with-xerj.md).
There, XERJ narrowed candidates and I read them. Here, XERJ is the agent's
**second brain** — external memory holding all 11,990 functions of real
WordPress — and the agent *reasons against it*: locate the auth primitives, read
how they actually work, build a model of correct-vs-buggy authorization, then
hunt the codebase for the buggy shape. Logic bugs live in the gap between "there
is a check" and "the check is correct," and that gap is only visible if you read
the real implementation.

No core vulnerability is claimed — core is the most-audited PHP alive, and it is
correctly gated. The deliverable is the **reasoning loop** and the **authz model
it produced**, which is exactly what transfers to plugin auditing where the real
bugs live.

## The loop (each step is a query the agent made, then reasoning)

### Step 1 — locate the gatekeepers

```
GET wpaudit  terms func:[current_user_can, wp_verify_nonce, check_ajax_referer,
                         map_meta_cap, wp_validate_auth_cookie]
```

The returned `calls` lists alone were a finding: `check_ajax_referer`'s callees
contain `wp_verify_nonce` but **no capability function**, and `wp_verify_nonce`'s
callees contain `wp_get_current_user`. Before reading a line of a body, the
second brain says: *nonces are user-bound; the ajax-referer check does no authz.*

### Step 2 — read the real implementation

`wp_verify_nonce` (wp-includes/pluggable.php): the token is

```php
substr( wp_hash( $tick . '|' . $action . '|' . $uid . '|' . $token, 'nonce' ), -12, 10 )
```

Bound to the 12-hour tick, the **action string**, the **user ID**, and the
**session token**. A valid nonce proves *this logged-in user deliberately made
this request*. It is CSRF defense — nothing more.

`check_ajax_referer` (same file): pulls the nonce from `$_REQUEST` and calls
`wp_verify_nonce`. **That is the whole function.** Zero authorization.

**Model fact #1: `check_ajax_referer()` authenticates the request's origin; it
never authorizes the action.** A Subscriber holds valid nonces. This single fact
defines a whole class of WP privilege-escalation bugs.

### Step 3 — learn what "correct" looks like

Reading the strong handlers taught the real pattern:

- `wp_ajax_trash_post`: `check_ajax_referer("{$action}_$id")` **and**
  `current_user_can('delete_post', $id)` — the nonce action and the capability
  are both **bound to the specific post `$id`**.
- `wp_edit_theme_plugin_file` (the RCE-class file writer):
  `current_user_can('edit_plugins')` **then**
  `wp_verify_nonce($nonce, 'edit-plugin_' . $file)` — the nonce action **includes
  the specific file**, so it cannot be replayed against a different file. Plus
  `validate_file()` for traversal.

**Model fact #2: WordPress's real defense is *object-scoped* meta-capabilities +
*object-scoped* nonces.** So "does the handler check a capability?" is the wrong
question. The right one is: **"is the capability bound to the object being acted
on?"** A generic `current_user_can('edit_posts')` plus a generic nonce is the
*buggy* shape — it lets a user act on objects they don't own. (This is the
pattern that recurs in plugin CVEs.)

### Step 4 — hunt the buggy shape across all 95 handlers

Core's authenticated AJAX handlers are all named `wp_ajax_<action>`:

```
GET wpaudit  prefix func:"wp_ajax_"   →  95 authenticated handlers
classify by direct callees:  nonce? capability?
```

| bucket | count |
|---|--:|
| nonce **and** capability (proper, direct) | 56 |
| nonce, **no** direct capability | 15 |
| **neither**, direct | 13 |

The 28 non-proper ones are the interesting set. The agent read the sensitive
ones and followed delegation edges:

- `wp_ajax_edit_theme_plugin_file` (in "neither") → delegates the entire check to
  `wp_edit_theme_plugin_file()`, which is the comprehensive gate above. **Cleared,
  and it taught the model.**
- `wp_ajax_untrash_post` (in "neither") → calls `wp_ajax_trash_post()`, the
  object-scoped gate. **Cleared.**
- `wp_ajax_closed_postboxes`, `hidden_columns`, `save_user_color_scheme`, … (in
  "nonce, no cap") → operate only on the **acting user's own preferences**. You
  are always allowed to edit your own state, so no capability is *needed*; the
  nonce (anti-CSRF) is the correct and sufficient control. **Cleared by
  understanding the operation, not by a rule.**

**A false-negative honesty note:** the join in Step 4 first returned *zero*
authenticated hooks, because core registers ajax actions dynamically
(`add_action("wp_ajax_$action", …)` in a loop) and the substrate's hook
extractor only captured *literal* hook strings. The agent noticed the impossible
result, reasoned about why, and pivoted to the naming convention. A silent
substrate gap is the real risk in this kind of work — it has to be caught by
sanity-checking results, exactly as here.

## Every "sink" in the flagged handlers was a false positive

Consistent with the [earlier run](wordpress-audit-with-xerj.md), and now with a
second cause identified:

- `require ABSPATH . WPINC . '/class-wp-editor.php'` → flagged **LFI**, but the
  path is a **constant**, not attacker input. (`wp_ajax_wp_link_ajax`,
  `wp_ajax_get_community_events`, and most core `require`s.)
- `echo wp_json_encode($results)` → flagged **XSS**, but it emits safe JSON as an
  ajax response, not HTML. (`wp_ajax_wp_link_ajax`.)
- `WP_Query->query()` / `WP_User_Query->get_results()` / `DOMXPath->query()` →
  flagged **SQL**, but none is `$wpdb`. (receiver-type blindness, from the earlier
  run.)

So the two concrete precision levers for the extractor are now clear:
**(a) resolve the receiver type of a `->method()` sink**, and **(b) treat
`require`/`include` of a constant/`ABSPATH`-rooted path as non-LFI.** Both are
mechanical; both would remove essentially all of the false positives seen across
both runs.

## Why this is the second-brain, not grep

Grep can find `check_ajax_referer` call sites. It cannot *read* `wp_verify_nonce`,
conclude "nonces don't authorize," derive that the correct pattern is
object-scoped meta-caps, and then use that derived model to triage 95 handlers
and follow each delegation edge to its real gate — all while holding a 619k-line
codebase in external memory and spending a few thousand tokens. The value isn't
retrieval; it's that **the agent can build and apply a model of the system's
security logic** because XERJ makes the whole system cheap to think against.

## Deepening: the interprocedural authz graph, and the true core is zero

The flat "does the handler check a capability?" classifier over-flags. The next
build (`wp_authz_graph.py`) enriches each function with the **argument shape** of
its cap/nonce checks and its state-change **call sites**, then propagates along
call edges. Three refinements — each forced by reading a real handler — took the
suspicious set from 7 to **0**:

1. **Self-scoped writes are decided at the call site.** `update_user_meta($user->ID, …)`
   where `$user = wp_get_current_user()` mutates the *session user's own* state —
   no capability required. (Fixes `closed_postboxes`, `hidden_columns`.)
2. **Polymorphic authorization counts.** `$wp_list_table->ajax_user_can()` is a
   real gate invisible to name-based cap detection. (Fixes `fetch_list`.)
3. **Trusted plumbing is an analysis boundary.** `check_ajax_referer → wp_verify_nonce
   → wp_hash → wp_salt` writes `update_site_option` (salt caching); `wp_generate_password
   → wp_rand → set_transient` (RNG seeding). These benign infra side-effects must
   not be attributed to the caller's authz surface — treat nonce/hash/RNG/cache
   primitives as leaves, exactly like the state primitives. (Fixes `generate_password`,
   `rest_nonce`, `dashboard_widgets`.)

Final sweep of all 95 authenticated AJAX handlers: **0** reach a non-self state
change or a request-identified object without an object-scoped/polymorphic cap.
Core's authenticated AJAX surface has no IDOR. Honest scope limit: this covers
`wp_ajax_*`; **REST controllers and `admin_post_*` are a separate surface** the
same graph must still sweep.

## A real XERJ finding: the code analyzer splits identifiers

Hunting the SQL flows surfaced a genuine limitation. XERJ's default `text`
analyzer tokenizes on underscore and punctuation: `esc_like` → `esc`+`like`,
`wp_unslash` → `wp`+`unslash`, `$wpdb->get_results(` → `wpdb`+`get`+`results`. So
`term`/`terms` queries for a multi-token identifier return **nothing**, and
`match` matches loosely (any sub-token). For code search this matters — you
cannot exactly distinguish `esc_like` from `esc … like`. The working pattern is
`match` for a coarse superset, then **exact regex over the returned `code`
field** for precision. A code-aware analyzer (keep `_`, keep `->`) is a concrete
autoindex improvement for the code use-case, and belongs on the same list as the
receiver-typed sinks.

## Verifying a real security flow: the SQL "escape-after-escape" de-escape

The dangerous class: a value escaped by one `prepare()`, then re-fed into another
`prepare()` — double-processing can turn a `%` in the value into a format
placeholder (placeholder injection / de-escape). The detector (prepared var
re-used inside another `prepare(`) found **one** instance in core:
`WP_List_Table::months_dropdown`, reachable from `$_GET['post_status']`:

```php
$extra_checks = $wpdb->prepare( ' AND post_status = %s', $_GET['post_status'] ); // inner
$wpdb->get_results( $wpdb->prepare(                                              // outer, interpolates $extra_checks
    "SELECT … WHERE post_type = %s  $extra_checks  ORDER BY …", $post_type ) );
```

Verified **safe**, by reading `prepare()` and `placeholder_escape()`:

- Inside `prepare()`, every literal `%` in a *value* is replaced with an
  unguessable `{hmac-sha256}` token (`placeholder_escape()`), not a `%`.
- So the inner prepare turns `$_GET['post_status'] = "X%s"` into
  `AND post_status = 'X{hmac}s'` — **no `%` survives**.
- The outer prepare's stray-`%` escaper and placeholder extractor therefore see
  nothing to misread; only its own `%s` (for `$post_type`) is a placeholder.
- At execution, a `query`-filter (`remove_placeholder_escape`, priority 0) swaps
  `{hmac}` back to a literal `%` inside the quoted string. No injection.

**The honest conclusion is the valuable one:** the double-prepare pattern is real
and *present in core*, and core is safe **only** because of the
`placeholder_escape()` hardening. Any plugin that escapes its own way —
`esc_sql` + manual quoting, `sprintf` in a loop, or `allow_unsafe_unquoted_parameters`
— bypasses that single defense and the identical pattern **de-escapes into
SQLi**. That is a flow the agent now understands end-to-end: the call site, the
`prepare()` internals, and the exact condition under which the defense is absent.

## The REST surface, and an OOP substrate bug found mid-sweep

REST is where core IDOR would most plausibly live, so the graph swept every
`*_permissions_check` in core (107 methods; 33 mutating, 57 read). The first
pass looked alarming — 24 mutating checks with "no object-scoped cap" — but
reading them exposed a **substrate bug, not a vulnerability**: every controller
defines a method named `get_item_permissions_check` / `update_item_permissions_check`,
and the call graph keyed edges by **bare method name**, so 40+ controllers
collapsed to one and `reach()` followed the wrong class's body. This is the OOP
analogue of the ambiguous-edge problem from the first scanner.

The fix — **resolve `$this->method()` to the same file (WP is one controller
class per file)** — made the reach trustworthy:

| REST surface | checks | missing object-scoped cap after fix |
|---|--:|--:|
| mutating (create/update/delete) | 33 | **0** real |
| read (get_item / get_items) | 57 | **0** real |

The residuals are all correct-by-design: per-user/private objects use
object-scoped meta-caps read one hop into a helper — `check_update_permission($post)`
→ `current_user_can('edit_post', $post->ID)`; app-passwords →
`current_user_can('read_app_password', $user->ID, $request['uuid'])` (a 3-arg
meta-cap); comments → `current_user_can('edit_comment', $comment->comment_ID)`.
What's left is *global* resources (themes, taxonomies, menus, widgets, settings)
where a site-level cap is the right gate, `create_*` (no pre-existing object to
bind), and cross-controller delegation (autosaves → the parent posts controller).

## Honest status: no missing-cap IDOR found in core AJAX or REST

Across the three surfaces swept rigorously — authenticated AJAX (95), REST
mutating (33), REST read (57) — **every state change or private-object read is
gated by an object-scoped meta-capability.** I did not find a missing-object-cap
IDOR in core. That is a real (negative) result, and it required three detector
refinements and two substrate-bug fixes (infra-boundary over-approximation;
OOP method-name collision) to state with confidence rather than as a guess.

Caveat on what this method *cannot* see: it verifies that an object-scoped cap is
**present** on the path. It does not catch **auth-bypass** bugs where the cap is
present but *evaded* — e.g. the WP 4.7.0 REST content-injection, where an `id`
type-juggle routed the checked-object and the mutated-object apart. That is a
different class (parameter/type confusion, not a missing check) and needs
taint/type reasoning on the object-resolution step, not a cap-presence sweep.

## The check-vs-use IDOR detector (the class cap-presence can't see)

Cap-presence proves a capability is *checked*; it cannot prove it is checked
against the **same object the operation acts on**. The real core-IDOR mechanism
(e.g. WP 4.7.0's `id` type-juggle) is exactly that mismatch: `permission_check`
resolves the object from request key **X**, the operation reads/acts on key **Y**,
and X ≠ Y — so a validated object and a touched object diverge.

`wp_checkuse_idor.py` detects the shape: for each controller, diff the
object-identifying request keys the `*_permissions_check` binds against the keys
the matching operation reads. On core it flags **12 candidates**; reading them
sorts into three buckets:

- **global resources** (templates, widgets, sidebars) — gated by a site cap
  (`edit_theme_options`), so no per-object binding is expected. Not a gap.
- **helper/delegated resolution** — the check binds the object one hop away
  (`get_items_permissions_check`, `parent_controller`), which the key-diff misses.
- **one genuinely risky shape — and core defends it explicitly.**

That last one is the find. `WP_REST_Revisions_Controller`: the permission check
binds the cap to `$request['parent']`
(`current_user_can('edit_post', $parent->ID)`), but `get_item` returns the
revision `$request['id']`. Pair a `parent` you can edit with an `id` belonging to
a post you can't, and you'd read its draft revisions — **unless the operation
re-verifies the relationship.** It does:

```php
$parent   = $this->get_parent( $request['parent'] );   // cap checked against this
$revision = $this->get_revision( $request['id'] );     // this is returned
if ( (int) $parent->ID !== (int) $revision->post_parent ) {
    return new WP_Error( 'rest_revision_parent_id_mismatch', …, array( 'status' => 404 ) );
}
```

Core is safe **only because of that explicit `parent_id_mismatch` guard**. The
detector found the exact structural shape; the read decided it. And the corollary
is the whole point:

> A plugin controller or AJAX handler that checks permission on one id but acts
> on another **without re-verifying the relationship** is a live IDOR. This
> detector flags that shape directly. Core survives by adding the consistency
> guard; plugins routinely omit it.

## Finishing core: the file-scope handlers and the authz engine

Three surfaces remained. Two exposed the last substrate gap.

**`admin_post_*`** — core registers essentially none by literal string (it is a
plugin surface); the hook extractor found 0. Noted, not a gap in core.

**Direct `wp-admin` page handlers** — the real gap. `wp-admin/post.php`,
`users.php`, `comment.php`, etc. handle requests in a **top-level
`switch($action)` at file scope**, not inside any function — so the
function-only index never saw a single one of them. `wp_admin_pages.py` extends
the substrate to extract each `case` block as a handler unit. It found 41
state-changing file-scope handlers; the cap-presence sweep flagged 24 without an
object-scoped cap. Reading the top ones showed **core is correctly gated** and
the flags are an extractor artifact — the guard lives in **shared preamble,
fall-through group heads, or a different nested switch**, not co-located with the
`case`:

- `users.php case 'delete'` → the real handler has
  `check_admin_referer('delete-users')` + `current_user_can('delete_user', $id)`
  per user; the extractor matched the *nested* `switch($_REQUEST['delete_option'])`
  `case 'delete'` (what to do with the deleted user's posts), not the authz case.
- `comment.php` delete/trash cases fall through to shared code guarded by
  `current_user_can('edit_comment', $comment->comment_ID)`.

The lesson repeats one level out: **authorization is frequently in shared scope,
not beside the action** — a file-scope handler analyzer must gather guards from
the enclosing switch/preamble, exactly as the interprocedural one gathers them
across call edges.

**`map_meta_cap` (the authz engine)** — fails safe. Meta-cap checked with no
object arg → `do_not_allow`; non-existent post → `do_not_allow`; revision →
resolve to parent, missing → `do_not_allow`. The single non-fail-closed fallback
(unregistered post type → `edit_others_posts`) still requires an editor-level
cap. Fail-closed by construction.

## Core is verified hardened; the detectors are ready

Every core authorization and SQL-escaping flow tested is object-scoped,
relationship-checked, and fail-safe:

| surface | result |
|---|---|
| authenticated AJAX (95) | object-scoped, 0 IDOR |
| REST mutating (33) + read (57) | object-scoped, 0 IDOR |
| file-scope wp-admin handlers (41) | object-scoped (guards in shared scope) |
| `map_meta_cap` engine | fails closed |
| check-vs-use (revisions) | defended by explicit consistency guard |
| double-prepare SQL de-escape | neutralized by `placeholder_escape` |

Getting these negatives *trustworthy* required 3 model refinements and 3
substrate-bug fixes (infra boundary, OOP method-name collision, file-scope
handler extraction) — each found by the agent noticing an impossible result and
reading the real code. That is the whole method: XERJ holds the system as
external memory; the agent builds a model, tests it, and repairs the model *and*
the substrate when reality disagrees.

## What's next

The detectors (object-scoped cap-presence, check-vs-use mismatch, double-prepare
de-escape, file-scope handler authz) are built and proven against a maximally
hardened core. They will actually fire on the **plugin ecosystem**, where
object-scoped caps, relationship re-verification, and placeholder-escape are the
exact defenses most often missing.
