# Findings

## Summary

| # | finding | class | severity | status |
|---|---|---|---|---|
| 1 | `wp_http_validate_url` allows `169.254.0.0/16` (cloud metadata) | SSRF (incomplete deny-list) | Medium (known-class) | **Real, verified, reachable** |
| 2 | `class-snoopy.php` curl build: `escapeshellarg($URI)` after flags, no `--` | option injection (escaper ≠ injection defense) | Informational (core) | **NOT reachable in core — bundled dead code**; valid pattern for plugins |
| 3 | `class-wp-image-editor-imagick.php` parses uploaded image content | ImageTragick (image-rce-ssrf) | Medium (deploy-dependent) | **Real surface** |
| 4 | `user-new.php:100` stores `$_REQUEST['role']` with NO `wp_ensure_editable_role()` (2 of 3 sibling role-sinks guard it) | role injection / privilege-escalation (inconsistent authz) | Medium (multisite + filtered `editable_roles`) | **Real gap — found by the per-file sweep I missed** |
| — | AJAX / REST / admin authorization | IDOR / privesc | — | Verified **clean** (object-scoped) |
| — | SQL double-`prepare` de-escape | SQLi | — | Verified **safe** (placeholder_escape) |
| — | sanitizer composition (escape→de-escape) | XSS/SQLi | — | Verified **safe** (escaper-last) |

Finding 1 is real+reachable; finding 3 is a real deploy-dependent surface; finding 2 is
NOT reachable in core (bundled dead code) — a valid pattern, plugin-relevant only; the rest are documented negatives — core is hardened — each confirmed
by reading the real implementation, not a pattern match. Findings 2 and 3 came
from the **agentic per-call audit**: the agent read each flagged call
(`escapeshellarg`, `Imagick::readImage`, `ZipArchive`) and wrote a verdict back to
`wpsinks` — proving the escaper-injection and class-based-sink classes are now
covered.

---

## Finding 1 — SSRF to cloud metadata via `wp_http_validate_url`

**Component:** `wp-includes/http.php` → `wp_http_validate_url()` — core's SSRF
gatekeeper for `wp_safe_remote_get/post/request()` and XML-RPC pingbacks.

**The gap.** The private-IP deny-list blocks loopback, `10/8`, `0/8`,
`172.16/12`, and `192.168/16` — but **not `169.254.0.0/16` (link-local)**, which
contains the cloud-metadata endpoint `169.254.169.254` (AWS/GCP/Azure) and
`100.100.100.200` (Alibaba). Also missing: CGNAT `100.64/10` and IPv6.

```php
if ( 127 === $parts[0] || 10 === $parts[0] || 0 === $parts[0]
    || ( 172 === $parts[0] && 16 <= $parts[1] && 31 >= $parts[1] )
    || ( 192 === $parts[0] && 168 === $parts[1] )
) { /* reject unless the http_request_host_is_external filter allows */ }
```

**How it was found — reading, not pattern-matching.** A structural scanner sees
"there is an IP check" and moves on. The bug is that the allow/deny list is
*incomplete*. The agent read the range coverage and noticed `169.254` was absent.
This became a reusable XERJ detector (`wp_ssrf_ranges.py`) that flags any SSRF
validator missing dangerous ranges — it reproduces the finding independently.

**Verified by executing the flow.** With no PHP runtime available, the function
was ported line-for-line to Python (same scheme filter, same
`strpbrk(host, ':#?[]')`, same octet test) and executed with real
`gethostbyname`:

| payload | executed verdict |
|---|---|
| `http://127.0.0.1/`, `http://10.1.2.3/`, `http://192.168.0.1/` | REJECTED |
| `http://[::1]/` | REJECTED (`strpbrk` catches `:`) |
| **`http://169.254.169.254/latest/meta-data/iam/security-credentials/`** | **ALLOWED** |

**Reachability traced to user input.** A gap only matters if input reaches it.
Callers of the safe-remote path that pass a caller-supplied URL:

- `WP_XMLRPC_Server::pingback_ping` — the pingback **source URL is
  attacker-supplied** (unauthenticated where XML-RPC is enabled). Classic SSRF
  vector; the `169.254` gap turns it into metadata access.
- `WP_REST_URL_Details_Controller::get_remote_url` — REST endpoint, user `url`
  parameter (author-level).
- oEmbed discovery, `download_url`, pingback discovery — all fetch caller URLs.

**Impact.** A request steered at `169.254.169.254` can read cloud IAM
credentials/tokens → account compromise, on hosts where the SSRF path is reachable
and a metadata service is present.

**Honest severity.** This is a **known-class** limitation. Core historically treats
`wp_http_validate_url` as best-effort and points hardening at the
`http_request_host_is_external` filter; exploitation needs the SSRF path enabled
in a cloud environment. It is **not** claimed as a novel 0-day. It is exactly the
kind of semantic-completeness weakness that only a *read* (not a pattern) finds,
which is the case-study point.

**Suggested remediation (defense in depth).** Add `169.254.0.0/16`,
`100.64.0.0/10`, and IPv6 loopback/ULA to the default deny-list; resolve and
re-check the IP at request time (mitigates DNS-rebinding TOCTOU).

---

## Finding 2 — `escapeshellarg` option injection (class: escaper-is-not-injection-protection)

`wp-includes/class-snoopy.php` (legacy vendored HTTP lib) builds a curl command:
```php
$cmdline_params = '-k -D '.escapeshellarg($headerfile).' -H '.escapeshellarg($header)...
exec($this->curl_path.' '.$cmdline_params.' '.escapeshellarg($URI), ...);
```
`$URI` is `escapeshellarg`-escaped but placed **after the flags with no `--`
separator**. `escapeshellarg` prevents shell *command* breakout — it does **not**
prevent curl from parsing the value as an **option**. If `$URI` is
attacker-controlled and starts with `-`, e.g. `-o/var/www/html/x.php` (file write →
RCE) or `-K /etc/passwd`, curl obeys it. **Honest scope:** Snoopy is legacy and
core rarely routes attacker URLs to it; the point is the *class* — an escaper that
is not injection protection. **Fix:** insert a literal `--` before `$URI`; reject
leading `-`; use argv-form. This is the pattern `option-injection` in the guide.

**Reachability trace (added after the finding):** traced `$URI` back to source — `new Snoopy` appears **nowhere** in core, `class-snoopy.php` is **never require()'d**, no core code calls `->fetch()/->submit()`, and `curl_path` is only ever the hardcoded default. So the `exec` line is **dead code in core** — `$URI` has no path from any `$_GET/$_POST`. **Re-classified: informational for core; the option-injection *class* is real and applies to any plugin that instantiates Snoopy (or builds an exec command) with a request-controlled URL.** This is the value of tracing input vars: it separated a valid pattern from a core-reachable bug.

## Finding 3 — Imagick processes uploaded image content (class: image-rce-ssrf)

`wp-includes/class-wp-image-editor-imagick.php` calls `Imagick::readImage($this->file)`
/ `readImageBlob(file_get_contents($this->file))` on **attacker-uploaded** images.
This is the ImageTragick surface (CVE-2016-3714 and kin): a crafted MVG/MSL/SVG
image can make an unpatched/mis-configured ImageMagick perform SSRF or command
execution via delegates. **Honest scope:** mitigated by a modern ImageMagick +
a restrictive `policy.xml`, which WP relies on the host to provide; WP checks file
type first but the content is still parsed. **Fix (deploy):** patched ImageMagick;
`policy.xml` disabling `MVG/MSL/URL/EPHEMERAL/HTTPS` coders.

## Finding 4 — inconsistent `wp_ensure_editable_role()` -> role injection (`user-new.php`)

**Found by the multi-agent per-file zero-day sweep — the structural authz graph
missed it** (it saw a `promote_user` cap check and passed; it did not check that
the *role-editability* guard was applied consistently).

`wp-admin/user-new.php` has **three** sinks that assign a request-supplied role.
Two are guarded, one is not:
```php
// line 73  (adduser, noconfirmation branch)   -> GUARDED
wp_ensure_editable_role( $_REQUEST['role'] ); add_existing_user_to_blog([... 'role'=>$_REQUEST['role']]);
// line 231 (createuser branch)                -> GUARDED
wp_ensure_editable_role( $_REQUEST['role'] );
// line 95-102 (adduser, email-invitation else branch)  -> NOT GUARDED
add_option( 'new_user_'.$key, [ 'user_id'=>$user_id, 'email'=>..., 'role'=>$_REQUEST['role'] ] );
$roles = get_editable_roles(); $role = $roles[ $_REQUEST['role'] ];   // AFTER the store; email only; warning not wp_die
```
The stored role is later applied on confirmation: `/newbloguser/{key}/` ->
`maybe_add_existing_user_to_blog()` -> `add_existing_user_to_blog()` ->
`add_user_to_blog()` -> `$user->set_role($role)` — **with no re-check** (only the
default-true `can_add_user_to_blog` filter). So a role the inviter is not
authorized to assign is persisted and applied.

**Reachability:** authenticated multisite user with `promote_user` on the target
but **not** `manage_network_users` POSTs `action=adduser` (valid `add-user` nonce),
an existing user's email, `role=administrator`, no `noconfirmation` -> stored ->
invitee visits the link -> becomes administrator.

**Exact trigger (traced to the HTTP request):**
```
POST /wp-admin/network/user-new.php   (multisite; also wp-admin/user-new.php)
Cookie: <logged-in site admin: has create_users + promote_users, NOT manage_network_users>
Content-Type: application/x-www-form-urlencoded

action=adduser
&_wpnonce_add-user=<valid nonce from the Add-User form>
&email=existing.network.user@example.com      # must resolve via get_user_by('email')
&role=administrator                            # <-- injected; NOT in the inviter's editable_roles
                                               # (omit 'noconfirmation' so the email-invitation else-branch runs)
```
Server stores `new_user_{key} = { user_id, email, role:'administrator' }` (line 100,
**no `wp_ensure_editable_role`**). Second-order trigger — the invitee clicks:
```
GET /newbloguser/<key>/     # init hook maybe_add_existing_user_to_blog -> add_user_to_blog -> set_role('administrator')
```
No re-check on confirmation. Conditions to actually cross a boundary: (a) multisite,
(b) an `editable_roles` filter that restricts this inviter below `administrator`
(else `wp_ensure_editable_role` is a no-op and the role was assignable anyway).

**Honest severity — conditional, defense-in-depth in stock.** In a *stock* install
`get_editable_roles()` returns all roles, so `wp_ensure_editable_role` is a no-op
and the inviter could assign that role anyway — **no boundary is crossed**. The gap
bites where **`editable_roles` is filtered** to restrict an admin below the injected
role (common in membership / role-manager plugins and multisite delegation), and
only on **multisite**. It is a genuine, unconditional *inconsistency* (2 of 3 sinks
guarded) matching upstream 7.0.2 — worth reporting to WordPress as a hardening fix,
not a stock-install RCE. Verified by reading the real code + adversarial agent. Full step-by-step attack (requests + code) in [ATTACK-SCENARIO-role-injection.md](ATTACK-SCENARIO-role-injection.md).

## Verified-clean negatives (the honest majority)

### Authorization / IDOR — clean
- **AJAX (95 handlers), REST (33 mutating + 57 read):** every state change and
  private-object read is gated by an **object-scoped meta-capability**
  (`current_user_can('edit_post', $post->ID)`, `read_app_password` with
  `$user->ID`+`uuid`, `edit_comment` with `$comment->comment_ID`).
- **check-vs-use IDOR** (cap checked on object A, operation on object B): the only
  risky shape (`WP_REST_Revisions_Controller`: cap on `parent`, returns `id`) is
  explicitly guarded — `if ($parent->ID !== $revision->post_parent) return 404`.
- **`map_meta_cap`** fails closed on missing object / missing arg.

### Deserialization POP-gadget chains — none in core
131 magic methods; **0** auto-triggered ones (`__wakeup`/`__unserialize`/
`__destruct`/`__toString`) reach a dangerous sink. The 2 that reach any dangerous
call are `__callStatic` (deprecation shim → WP hook dispatcher) — false positives,
and not unserialize-triggered. So even arbitrary object injection has **no core
gadget chain** to escalate. See `GADGET-CHAINS.md`.

### SQL de-escape (double `prepare`) — safe
`WP_List_Table::months_dropdown` re-feeds a `prepare()`'d value (from
`$_GET['post_status']`) into another `prepare()`. Safe **only** because
`placeholder_escape()` converts value-`%` to an unguessable `{hmac}` token,
restored at execution. Plugins that self-escape bypass this one defense and the
same pattern becomes SQLi.

### Sanitizer composition — safe
Core holds the **escaper-last-before-sink** invariant everywhere
(`get_terms`: `stripslashes` → `esc_sql` last; `wp_update_term`: `wp_unslash`
before the self-escaping `$wpdb->update`). 23 order-flagged candidates all cleared
on reading.

---

## Meta-finding: a real XERJ engine bug the audit surfaced

The reverse call-graph query "who calls `wp_safe_remote_get`" returned **1** from
XERJ's structured `term` index vs **9** (full-text) / **14 files** (grep). Root
cause: `term`/`terms` on a keyword **array** matched only element `[0]` (single-
valued keyword storage). Proven, fixed (memtable half), and written up as a PR:
[`../../research/xerj-keyword-array-term-fix.md`](../../research/xerj-keyword-array-term-fix.md).
A false "not reachable" is the worst error in an audit — surfacing it is part of
what makes this method trustworthy.
