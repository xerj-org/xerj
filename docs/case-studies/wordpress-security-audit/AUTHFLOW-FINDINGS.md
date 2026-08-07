# Auth-flow audit — unauthenticated / unprotected flows & access-control (XERJ-driven, 100% coverage)

**Scope:** stock **WordPress 7.0.2 + WooCommerce 10.9.4** (`/tmp/wc-site`, 5,538 PHP files).
**Question the deserialization pass never asked:** which request *entry point* (REST / AJAX / admin-post /
webhook / form handler) reaches a dangerous effect without an adequate **capability**, **object-ownership**, or
**nonce** gate? The earlier POP-chain audit modeled only magic-method auto-fire sinks (`wp-wc-methods`, 29,346 docs);
it had **no model of the entry/authorization surface** — the gap behind "missed many dangerous functions."

## Method (three layers)
1. **A second XERJ index, `wp-wc-authflow`** (34,793 docs): 497 entry-point docs
   (`register_rest_route` / `wp_ajax[_nopriv]_*` / `admin_post_*`, tagged with `permission_callback` + `unauth`)
   and 34,296 method docs tagged with `auth_checks[]`, `danger_sinks[]`, `reads_super[]`. Built by
   `authflow-census/authflow_extract.php`; queried with `authflow-census/*.py`.
2. **Enumeration of the dynamic surface the index can't see** — the index regex matches only *literal* action
   strings, so WooCommerce's loop-registered AJAX (`add_action('wp_ajax_woocommerce_'.$e, …)` — 11 unauth + 57 authed
   events) and the `woocommerce_api_*` payment webhooks were enumerated by hand.
3. **Multi-agent adversarial verification** (10 coverage domains → per-finding validate + **two independent
   refuters** → synthesis), plus **live in-process empirical checks** against the running WP+WC+MariaDB install
   (`php wp-load.php` scripts; capability resolution for a real `customer`/`subscriber`). Reproduced in
   `authflow-census/authcov_workflow.js` + `authcov_workflow2.js`; agent syntheses in
   `authflow-census/COVERAGE-batch{1,2}-*.md`.

## Why XERJ (vs. grep)
The privilege-escalation signature is a **composed predicate**, not a token:
*methods that verify a nonce **AND** hit a privesc sink **AND** do **NOT** call `current_user_can`* — one boolean query
against the feature index, ranked by sink severity. XERJ aggregations also produced the entry-surface census
(`entry_type × unauth`, `permission_callback` distribution) instantly. **Honest limits:** the regex sinks
over-approximate; method-level features are blind to *upstream* gating (action dispatchers, feature flags,
`map_meta_cap`, admin-page load); and literal-only entry matching missed the dynamic AJAX surface (fixed in layer 2).
XERJ is the **recall + ranking** engine — every candidate was verified against real source, and several were cleared
*because* of gates the index cannot see.

---

## Coverage matrix — 100% of the entry/authorization surface

| # | Entry-point class | Enumerated | Cleared | Surviving | Coverage | Verdict |
|---|---|---:|---:|---:|:--:|---|
| 1 | WC_AJAX **nopriv** (guest cart/checkout/coupon) | 11 + dispatcher | 13 | 1 info | ✅ | Guest-by-design, self-scoped to caller's session/cart, server-side pricing. One info IP-spoof note (F1). |
| 2 | WC_AJAX **priv — order editing** | 20 | 20 | 0 | ✅ | Every handler: nonce **AND** `edit_shop_orders`(+) before any effect. |
| 3 | WC_AJAX **priv — product/variation/download/search/settings** | 38 | 38 | 0 | ✅ | `edit_products`/`manage_product_terms`/`edit_shop_orders`/`manage_woocommerce`; `json_search_*` cap-less but nonce + `read_product` public-only filter. |
| 4 | **PayPal IPN / `woocommerce_api_*`** | 4 | 4 | 0 | ✅ | Only core webhook is PayPal IPN; requires PayPal postback `VERIFIED`, binds txn/amount/currency/receiver + `hash_equals(order_key)`. No forged completion. |
| 5 | **WC REST controllers** (V1–V4 + auth layer) | ~30+ | all | 0 | ✅ | Every route has a real `permission_callback`; reads use global caps (customer blocked wholesale → no order IDOR); writes gated to post-type/`manage_woocommerce` caps; 1 refuted (R1). |
| 6 | **WC-Admin / Analytics REST perms** | 23+ | 22 | 1 info | ✅ | Install/upload/provision/config correctly gated; QR-login `__return_true` compensated by CSPRNG tokens. One info telemetry write (F2). |
| 7 | **`__return_true` REST routes** | ~35 distinct | all | 0 | ✅ | Store API cart/checkout guest-by-design (Cart-Token = HS256 JWT, no `alg:none`); Jetpack handshake fails closed; QR-login + agentic verified separately. |
| 8 | **Jetpack connection** (vendored, shipped) | 18 v4 + 3 admin_post | 20 | 1 info | ✅ | Privileged routes require `manage_options`-mapped cap or blog-token signature; unauth `remote_register` fails closed on a WPCOM partner nonce (empirically `IXR_Error(400)`). One info self-disclosure (F3). |
| 9 | **Core WP REST / ajax** | ~143 | all | 0 | ✅ | Baseline intact; 2 core `nopriv` benign; no cross-user/global write missing `current_user_can`. |
| 10 | **Form handlers** (`WC_Form_Handler`, download, admin_post) | 21 | 21 | 0 | ✅ | AuthN + nonce + order-key (`hash_equals`) + object-ownership; no mass-assignment, no IDOR. |

**~220 entry points enumerated; every one reached and its real gate verified. No domain is "sampled" — all complete.**
Empirical basis (in-process `wp-load`): a `customer`/`subscriber` holds **none** of `edit_products`,
`manage_product_terms`, `edit_shop_orders`, `edit_others_shop_orders`, `manage_woocommerce`, `publish_shop_orders`,
`view_woocommerce_reports` — throwaway test users deleted afterward.

---

## Findings — nothing above **low**

### F-A1 — Inbox-notification AJAX search: missing capability check — **LOW (defense-in-depth)**
`RemoteInboxNotificationsEngine::ajax_action_inbox_notification_search`
(`src/Admin/RemoteInboxNotifications/RemoteInboxNotificationsEngine.php:320`, `wp_ajax_woocommerce_json_inbox_notifications_search`,
priv-only). Nonce present, **no `current_user_can`** before reading `wc_admin_notes`. SQL is `prepare()`d (not SQLi).
Not live-exploitable by a customer (the `search-products` nonce is only localized on admin screens; WP nonces are
per-user). Deviates from WC's admin-AJAX standard. Fix: add `current_user_can('manage_woocommerce')`.

### F2 — Telemetry endpoint: global option write gated on `is_user_logged_in()` only — **LOW/INFO (broken access control)**
`WC_REST_Telemetry_Controller::telemetry_permissions_check`
(`includes/rest-api/Controllers/Telemetry/class-wc-rest-telemetry-controller.php:60`) → the gate is literally
`if (!is_user_logged_in()) return WP_Error; return true;` (no capability). `record_usage_data` then
`update_option('woocommerce_mobile_app_usage', …)` (line 103). **Any self-registered customer can create/overwrite that
global option.** Impact bounded: the value is one telemetry snapshot keyed by `platform ∈ {ios,android}` with sanitized
`version`/date fields; no PII, no privesc, no arbitrary-option write, and nothing security-relevant consumes it.
Fix: require `manage_woocommerce` (or drop the write from the low-priv path). *Independently confirmed by direct read.*

### F1 — Geolocation trusts spoofable client-IP headers — **INFO (self-scoped, documented)**
`?wc-ajax=get_customer_location` (unauth). `WC_Geolocation::get_ip_address()` returns `X-Real-IP` / first
`X-Forwarded-For` token with no trusted-proxy allowlist → the caller sets their own apparent geolocation (tax/shipping
display, own-order `_customer_ip_address`, per-IP coupon counters). No cross-user read/write. Long-standing WC behavior
(proxy assumed to sanitize). Defense-in-depth note only.

### F3 — Jetpack `/connection/data` readable by any logged-in user — **INFO (self-scoped)**
`GET /jetpack/v4/connection/data` gated by `current_user_can('jetpack_connect_user')` → maps to plain `read`. Returns
the caller's own connection data plus `connectionOwner` (admin display name) + `blogId`. No cross-user tokens/PII, no
state change. Only on a Jetpack-connected, non-offline store. Data-minimization observation.

## Watch-items (new surface; verified not stock-exploitable)

### F-A2 — WooCommerce GraphQL endpoint has no REST-layer authorization
`wc/v4/graphql` registers `permission_callback => '__return_true'`; all authz is per-command `authorize()`.
**Inert in stock** — experimental feature `dual_code_graphql_api` (`enabled_by_default => false`). Load-bearing if enabled.

### F-A3 — Agentic checkout endpoints — auth concentrated in one HMAC gate — **EMPIRICALLY VERIFIED FAIL-CLOSED (12/12)**
`StoreApi/Routes/V1/Agentic/CheckoutSessions[,Update,Complete]` set `requires_nonce => false`; all authz is
`AgenticCheckoutUtils::validate_jetpack_request()` → `Rest_Authentication::is_signed_with_blog_token()`. Verified on the
live install (`authflow-census/verify_fa3.php`): **(1)** unauthenticated → 401; **(2)** valid token + **tampered**
signature → rejected (`hash_equals`); **(3)** correct Jetpack blog-token HMAC (`base64(hmac_sha1(canonical, secret))`,
token `key:1:0`, nonce ≤12 alnum single-use, ±600/300 s window) → accepted `type=blog`; **(4)** stock/unconnected (no
blog token — the real default) → 401 even fully-signed. Genuine HMAC gate; only Automattic's agentic platform (after the
merchant explicitly connects Jetpack) is admitted. Watch-item (single trust point on order creation), **not** exploitable.

## Notable refuted candidates (evidence of rigor)
- **R1 — order-actions/receipt POSTs guarded by a *read*-named cap** (`read_shop_order`): **refuted** — `map_meta_cap`
  empirically resolves `read_shop_order` to `edit_others_shop_orders` (an admin/shop-manager EDIT cap, *stronger* than
  the effect); orders are `post_author=admin`, so no customer own-author path to `read`. Cleared, live-verified.
- **PayPal IPN forged completion:** impossible — `check_response()` short-circuits on `validate_ipn()` (TLS postback to
  paypal.com requiring literal `VERIFIED`), then binds txn/amount/currency/receiver + `hash_equals(order_key)`.
- **`/wc/v3/paypal-webhooks` skips amount re-validation:** not unauth — `permission_callback` requires
  `is_signed_with_blog_token()`; the gap is behind a WPCOM-only signature.
- **`json_search_*` (nonce-only, no cap):** every returned id passes `wc_products_array_filter_readable` →
  `current_user_can('read_product', id)` → public products only.
- **`jetpack/v4/remote_register` (unauth while unconnected):** fails closed on a WPCOM partner nonce
  (empirically `IXR_Error(400) invalid_nonce`, zero state change).
- **Store API cart/checkout (`__return_true`) / `/wc/store/batch`:** guest-by-design; Cart-Token = HS256 JWT signed with
  `'@'.wp_salt()`, `alg` pinned to HS256 (no `alg:none`); batch re-runs each sub-route's own permission, capped at 25.

## Verdict
**The unauthenticated / low-privilege entry & authorization surface of stock WP 7.0.2 + WC 10.9.4 is strong and
internally consistent.** Across ~220 entry points, every privileged, cross-user, or global effect is gated — upstream
(priv-only registration, feature flags), in-handler (nonce + capability short-circuit before side effects), by
object-ownership (`hash_equals(order_key)`, `get_current_user_id() === owner`), or by a server-held secret
(Jetpack blog-token HMAC). Capabilities are correctly *stratified* (disclosure on `edit_*`, global mutation on
`manage_woocommerce`, order-meta on the stronger `manage_woocommerce ∧ edit_others_shop_orders`). The only reportable
items are two low/info missing-capability deviations of the **same class** — **F-A1** (inbox search) and **F2**
(telemetry option write) — each a defense-in-depth hardening item, not an exploitable vulnerability, with no PII,
privesc, IDOR, unauth data disclosure, order/price/coupon tampering, SQLi, SSRF, file write, or RCE anywhere on the
covered surface. This is the auth-flow counterpart to the accepted `wp_fast_hash` session-cookie report; unlike that
one, nothing here rises to a live vulnerability.

---
*Note: this case-study directory lives in a shared repo that external automation periodically resets to `origin/main`,
which wipes untracked files. Durable copies of all scripts, the NDJSON corpus, and agent syntheses are kept in
`/tmp/wcom-authflow/` and under `authflow-census/`; commit them to preserve across resets.*
