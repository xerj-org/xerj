# Whitebox Auth/Authorization Audit — WordPress 7.0.2 core + WooCommerce 10.9.4

**Scope:** Broken access control, IDOR/object-level auth, unauth-reachable dangerous effects across 10 entry-point classes.
**Method:** Fan-out enumeration across entry-point domains, real-source verification of every gate, in-process `wp-load` empirical capability checks against the live MariaDB/WC install, each candidate passed through two adversarial refuters (only `survived` findings shown).
**Bottom line up front:** No finding rises above **info**. The audited surface is uniformly and correctly gated. Details below.

---

## 1. Coverage Matrix

| # | Entry-point class | Enumerated | Cleared | Surviving | Coverage | One-line verdict |
|---|---|---:|---:|---:|---|---|
| 1 | **WC_AJAX nopriv** (11 guest handlers + dispatcher + 2 extra) | 14 | 13 | 1 (info) | **Complete** | By-design guest shopping; effects self-scoped to caller's own session/cart; server-side pricing. Only an info-level spoofable-client-IP note. |
| 2 | **WC_AJAX priv-orders** | — | — | — | **Not in this batch** | Not represented in the provided fan-out data. Adjacent order surface (REST orders/refunds/notes/actions/receipts, `edit_shop_orders`/`read_shop_order` meta-cap) was fully covered under class 5; the classic `wp_ajax_woocommerce_*` order handlers themselves were not returned here — treat as **sampled, not exhaustive.** |
| 3 | **WC_AJAX priv-data/settings** (25 handlers) | 24 | 24 | 0 | **Complete** | Every handler = nonce + adequate capability (`manage_woocommerce` / `edit_shop_orders` / `edit_products`) or nonce + object-level `read_product` filter. Customer/subscriber empirically hold none of the required caps. |
| 4 | **PayPal IPN / `woocommerce_api`** (3 completion paths + dispatcher) | 4 | 4 | 0 | **Complete** | Only one core `woocommerce_api_*` handler exists (PayPal IPN). All 3 order-completion paths validate back-channel to PayPal / require Jetpack blog-token / bind to secret `order_key`. No forged-completion path. |
| 5 | **WC REST controllers** (V1–V4, ~30+ controllers + auth layer) | 11 clusters | 11 | 0 | **Complete (grouped)** | Every route has a real `permission_callback`; reads use global caps (customer blocked wholesale → no customer→order IDOR); writes gated to `manage_woocommerce`/post-type caps; PushNotifications & Fulfillments enforce object-level ownership. 1 refuted (below). |
| 6 | **WC-Admin / Analytics REST perms** (23+ callbacks) | 14 clusters | 14 | 1 (info) | **Complete** | Install/upload/provision/config all correctly gated (`install_plugins`/`upload_themes`/`manage_woocommerce`). QR-login `__return_true` routes compensated by CSPRNG bearer tokens. Only info-level telemetry option-write. |
| 7 | **`__return_true` REST routes** (~35 distinct, 170 handlers live) | 12 clusters | 12 | 0 | **Complete** | 100% of live `__return_true` routes reached and classified. Store API cart/checkout is guest-by-design, session-bound, nonce-gated, Cart-Token = HS256 JWT (no `alg:none`). Jetpack handshake routes fail closed on server-generated secrets. |
| 8 | **Jetpack connection** (18 v4 routes + 3 admin_post) | 21 | 20 | 1 (info) | **Complete** | Every privileged route requires `manage_options`-mapped cap or `is_signed_with_blog_token()` (WPCOM-only, fails closed). Unauth-reachable `remote_register` fails closed on a WPCOM-issued partner nonce (empirically proven `IXR_Error(400)`, zero state change). Only info-level self-scoped disclosure. |
| 9 | **Core WP REST / ajax** | partial | partial | 0 | **Sampled** | Covered only where it intersects the WC surface: `wp/v2/users/me` (self-only), `wp/v2/types/{type}` (public schema), `oembed/1.0/embed` (public), `wp/v2/users/me` update (self-only). No dedicated core-WP fan-out in this batch — **not exhaustive.** |
| 10 | **Form handlers** (`admin_post_*` / `wp_loaded` POST) | partial | partial | 0 | **Sampled** | Only the 3 Jetpack SSO invite `admin_post` handlers were reached (all `check_admin_referer` + `create_users`/`promote_users`). WC's own `WC_Form_Handler` / checkout POST path was touched via the nopriv checkout trace but not enumerated as a class — **not exhaustive.** |

**Honest coverage summary:** Classes 1, 3, 4, 5, 6, 7, 8 are **complete** (every in-scope entry point reached and its real gate verified, most with live in-process capability checks). Classes 2 (WC_AJAX priv-orders), 9 (core WP REST/ajax), and 10 (form handlers) are **sampled, not exhaustive** — they are not represented as dedicated domains in this batch's data, and I will not invent results for them.

---

## 2. Confirmed Findings (surviving, most-severe first)

**There are no findings above `info` severity.** All three surviving items are info-level: correctly-designed behavior with negligible, self-scoped, or bounded impact. Stated plainly for the record:

### F1 — Geolocation trusts spoofable client IP headers (self-scoped)
- **Entry point:** `?wc-ajax=get_customer_location` / `wp_ajax_nopriv_woocommerce_get_customer_location`
- **Access class:** Unauthenticated
- **File:** `wp-content/plugins/woocommerce/includes/class-wc-geolocation.php:82` (`get_ip_address`); consumed at `class-wc-cache-helper.php:168` and `class-wc-ajax.php:633`
- **Gate present:** None (nopriv by design). `get_ip_address()` returns `HTTP_X_REAL_IP` verbatim, else the first token of `HTTP_X_FORWARDED_FOR`, with no trusted-proxy allowlist — the client fully controls the resolved IP.
- **Effect:** Attacker sets their own apparent geolocation (tax/shipping/currency display), and can influence the `_customer_ip_address` stamped on their own order and per-IP coupon rate counters. **No cross-user read or write.**
- **Severity:** info
- **Attack:** `curl 'http://localhost:8200/?wc-ajax=get_customer_location' -H 'X-Forwarded-For: 1.2.3.4'`
- **Residual condition:** Only meaningful if the site is *not* behind a reverse proxy that overwrites these headers AND the store relies on IP for coupon limits/fraud logging. Impact is anti-abuse-evasion / self-affecting, not cross-user. Long-standing documented WooCommerce behavior (proxy is assumed to sanitize headers).

### F2 — Telemetry endpoint gates a global option write on `is_user_logged_in()` only
- **Entry point:** `POST /wp-json/wc-telemetry/tracker` (`WC_REST_Telemetry_Controller::telemetry_permissions_check`)
- **Access class:** Any logged-in (i.e. any self-registered customer)
- **File:** `woocommerce/includes/rest-api/Controllers/Telemetry/class-wc-rest-telemetry-controller.php:60`
- **Gate present:** `is_user_logged_in()` only — no capability check.
- **Effect:** Any customer can create/overwrite the global `woocommerce_mobile_app_usage` option (platform/version/first_used/last_used snapshot in the WC Tracker payload). Global data-integrity pollution; **no PII disclosure, no privilege escalation.**
- **Severity:** info (a genuine broken-access-control — a customer writing a site option — but the writable surface and downstream use are minimal)
- **Attack:** Self-register → obtain REST nonce → `POST` with `platform=ios&version=9.9.9&installation_date=...`; repeat with increasing version to keep overwriting.
- **Residual condition:** `platform` constrained to `ios`/`android`, `version`/`date` sanitized; the value only feeds the mobile-app-usage tracker snapshot and is never treated as trusted for a security decision.

### F3 — Jetpack `/connection/data` + `/authorize_url` readable by any logged-in user (self-scoped)
- **Entry point:** `GET /wp-json/jetpack/v4/connection/data` and `/authorize_url` (`REST_Connector::user_connection_data_permission_check`)
- **Access class:** Any logged-in
- **File:** `woocommerce/vendor/automattic/jetpack-connection/src/class-rest-connector.php:1069` (perm) / `:618` (handler); cap mapping `class-manager.php:1634`
- **Gate present:** `current_user_can('jetpack_connect_user')`, which maps to the plain `read` capability when `has_connected_owner()` is true and the site is not offline — any customer holds `read`.
- **Effect:** Caller reads their *own* connection data plus two shared fields: `connectionOwner` (admin display_name) and `blogId` (WPCOM site id). No cross-user tokens/PII, no state change.
- **Severity:** info
- **Attack:** Self-register → grab `wp_rest` nonce from any front-end page → `GET` with cookie + `X-WP-Nonce`.
- **Residual condition:** Requires the store to be Jetpack-connected AND not in offline/local mode (this local install can't reach that state; `is_offline_mode=true`). Disclosed data is low-sensitivity and intentionally exposed for the non-admin self-connect UI. Not IDOR (all per-user data is the caller's own).

---

## 3. Notable Refuted Candidates (evidence of rigor)

### R1 — "State-changing order-actions/receipt POSTs authorized by a READ capability" — **REFUTED**
- **Entry point:** `POST .../order-actions/orders/<id>/actions/send_order_details` & `/send_email`; `POST .../order-receipts/orders/<id>/receipt`
- **Why it looked bad:** Both state-changing POST routes gate on `check_permission($request,'read_shop_order',$order_id)` — a *read*-named capability guarding a write (email dispatch / billing-email edit).
- **Why it's actually safe (verified live in-process):** `read_shop_order` is a **meta-cap**, and for `shop_order` posts it does *not* map to the site-wide `read` primitive. `map_meta_cap('read_shop_order', customer, order)` empirically resolves to `[edit_others_shop_orders]` — an admin/shop_manager EDIT capability — for the owning customer, a second attacker-customer, and across all order statuses. `current_user_can('read_shop_order', id) = false` for the owning customer and every other customer. Orders are stored `post_author=1` (admin), so the `map_meta_cap('read_post')` own-author path to plain `read` is never taken. `wc_customer_has_capability` grants customers only `view_order`/`pay_for_order`/`order_again`/`cancel_order`/`download_file` — never `read_shop_order`. Endpoints are unreachable by unauth and by customers; a legitimate caller must already hold `edit_others_shop_orders`, for whom emailing order details is in-scope. **Belongs in cleared.**

### Other high-interest items cleared with proof (not false alarms, but worth flagging as rigor):
- **`woocommerce_api_wc_gateway_paypal` (unauth IPN):** Forged order completion impossible — `check_response()` `&&`-short-circuits on `validate_ipn()` which posts back to `paypal.com` via TLS-verified `wp_safe_remote_post` and requires a literal `VERIFIED`; then binds txn type/currency/amount/receiver_email to the order and resolves the order via `hash_equals()` on the secret `order_key`; replay blocked by paid-status guard.
- **`POST /wc/v3/paypal-webhooks` (handler skips amount/currency re-validation):** Not unauth — `permission_callback` requires `is_signed_with_blog_token()` (Jetpack HMAC shared secret), fails closed if Jetpack unavailable. Real amount-validation gap is behind a WPCOM-only signature.
- **`json_search_products` family (nonce-only, no capability):** Every returned id passes `wc_products_array_filter_readable` → `current_user_can('read_product', id)`, which a customer empirically passes only for **published** products (draft/private denied) → public data only.
- **`jetpack/v4/remote_register` (unauth-reachable while unconnected):** Handler fails closed on a WPCOM partner-provision nonce validated by a live call to WPCOM; empirically returns `IXR_Error(400) invalid_nonce`, `Jetpack_Options` id unchanged.
- **Store API `/wc/store/*` cart/checkout (`__return_true`):** Guest-by-design; Cart-Token = HS256 JWT signed with `'@'.wp_salt()`, `JsonWebToken::validate` enforces `alg=HS256` (no `alg:none`) via `hash_equals` — cross-session hijack unforgeable; checkout order is session-bound draft, no request-controlled `customer_id`.
- **`/wc/store/batch`:** Re-dispatches through `serve_batch_request_v1`, which re-runs each sub-route's own permission + nonce; grants nothing the individual routes don't; capped at 25.

---

## 4. Bottom-Line Verdict

**Auth/authorization posture of stock WP 7.0.2 + WC 10.9.4: strong and internally consistent.** Across the seven domains covered exhaustively (nopriv AJAX, priv data/settings AJAX, PayPal IPN, WC REST controllers, WC-Admin/Analytics perms, `__return_true` routes, Jetpack connection), **every privileged, cross-user, or global effect is gated by an adequate capability, an object-level ownership check, or a server-held secret** — and the guest-facing surfaces (cart/checkout/catalog) are correctly self-scoped with server-side pricing and CSRF nonces. Live in-process capability checks confirmed that a self-registered `customer`/`subscriber` holds only `read` and passes none of the privileged meta-caps, including for orders (stored `post_author=admin`, so no customer→order path). The one architectural subtlety that looks alarming — read-named meta-caps guarding order writes — was verified to resolve to edit-level caps, i.e. *stronger* than the effect, not weaker.

**Worth reporting to the WooCommerce security team — honestly framed:**
- **F2 (telemetry option write, `is_user_logged_in()`-only):** The single genuine broken-access-control primitive: any authenticated low-priv user can write the global `woocommerce_mobile_app_usage` option. **Report as a low/info hardening item**, not a vulnerability — recommend adding a capability check (`manage_woocommerce`) or removing the write from the low-priv path. No PII, no escalation, bounded sanitized value.
- **F1 (spoofable geolocation IP):** Known, documented, proxy-assumed behavior. Worth a **defense-in-depth note** (optional trusted-proxy allowlist) but not a reportable vulnerability.
- **F3 (Jetpack connection-data disclosure):** Low-sensitivity, intentionally exposed for the self-connect UI, and only on connected non-offline stores. Not worth a report beyond a data-minimization observation.

**Nothing exploitable for privilege escalation, IDOR, unauth data disclosure, order/price/coupon tampering, SQLi, SSRF, file write, or RCE was found on the covered surface.** The honest residual risk is not in what was found but in what was **sampled, not exhausted**: WC_AJAX priv-*orders* handlers, the broad core-WP REST/ajax surface, and WC's own form/`admin_post` handlers were not enumerated as dedicated domains in this batch and should be closed out before declaring full coverage.