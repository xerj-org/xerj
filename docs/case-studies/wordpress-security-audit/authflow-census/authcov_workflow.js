export const meta = {
  name: 'wc-auth-coverage',
  description: 'Exhaustive auth/authorization coverage sweep of WP7.0.2+WC10.9.4 entry points, adversarially verified',
  phases: [
    { title: 'Analyze', detail: 'one agent per uncovered entry-point domain' },
    { title: 'Verify', detail: 'adversarial refuters per candidate finding' },
    { title: 'Synthesize', detail: 'coverage matrix + final findings' },
  ],
}

const PREAMBLE = `
You are a senior application-security auditor doing a WHITEBOX audit of a REAL, RUNNING install:
- Source tree: /tmp/wc-site  (WordPress 7.0.2 core + WooCommerce 10.9.4 plugin, 5,538 PHP files). READ real code here.
- Live install is bootable IN-PROCESS for empirical checks: write a short PHP script that does
  \`$_SERVER[...]; define('WP_USE_THEMES',false); require '/tmp/wc-site/wp-load.php';\` then call functions, and run \`php script.php\`.
  MariaDB is up (db wpc/wpc@127.0.0.1, siteurl http://localhost:8200). DO NOT run a long-lived \`php -S\` server (it is killed by the sandbox, exit 144/sig16). Short-lived \`php\` scripts are fine. If you write to options, DELETE them afterward.
- Structured indexes on http://127.0.0.1:9200 (Elasticsearch-compatible): index \`wp-wc-authflow\` (entry points + method auth/sink features) and \`wp-wc-methods\` (deserialization sinks). Query with curl if useful.

THREAT MODEL (what counts):
- UNAUTHENTICATED attacker (no login). A WooCommerce store also lets ANYONE self-register as a 'customer', so "any logged-in user" ≈ attacker. Treat a 'customer'/'subscriber' as hostile.
- A WordPress NONCE is NOT authorization — it is CSRF protection, per-(user,action,session). Distribution matters: an admin-only nonce a customer can't obtain is a de-facto gate; a nonce localized on a customer-facing page is not.
- Look for: MISSING capability check on a privileged/cross-user/global effect (broken access control / privilege escalation); OBJECT-LEVEL auth gaps (IDOR — checks "logged in" but not "owns this object"); UNAUTH reaching a dangerous effect (state change, PII/data disclosure, SQLi, SSRF, file write/delete, RCE, order/price/coupon tampering, auth bypass, account takeover); permission callbacks that are effectively \`__return_true\`, only \`is_user_logged_in\`, or a capability too low for the effect.

DISCIPLINE (this codebase is well-hardened — be precise, not credulous):
- VERIFY reachability and the ACTUAL gate by reading real source. Capability checks are often UPSTREAM (in an action dispatcher, a base controller, a feature-flag, or the admin-page load) — follow the call chain before concluding a gate is missing.
- For every candidate, state the concrete attacker request, the exact missing/weak gate, and the effect. Give a severity and a residual_condition (what must be true for it to bite).
- Prefer FEWER, HIGHER-CONFIDENCE findings. A cleared item (with the reason it's safe) is a valid, valuable result — report those in \`cleared\`.
`;

const FINDINGS_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['domain', 'coverage_note', 'findings', 'cleared'],
  properties: {
    domain: { type: 'string' },
    coverage_note: { type: 'string', description: 'What entry points you enumerated and confirmed you covered (counts), and any you could NOT reach.' },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['title', 'entry_point', 'access_class', 'file', 'handler', 'gate_present', 'effect', 'severity', 'attack', 'residual_condition'],
        properties: {
          title: { type: 'string' },
          entry_point: { type: 'string', description: 'e.g. wp_ajax_nopriv_woocommerce_checkout, POST /wc/v3/..., woocommerce_api_wc_gateway_paypal' },
          access_class: { type: 'string', enum: ['unauthenticated', 'any_logged_in', 'low_priv_role', 'nonce_only', 'other'] },
          file: { type: 'string', description: 'file:line of the handler / gate' },
          handler: { type: 'string' },
          gate_present: { type: 'string', description: 'the actual auth/authz/validation present (or "none")' },
          effect: { type: 'string', description: 'dangerous effect reachable' },
          severity: { type: 'string', enum: ['info', 'low', 'medium', 'high', 'critical'] },
          attack: { type: 'string', description: 'concrete request/steps' },
          residual_condition: { type: 'string' },
        },
      },
    },
    cleared: {
      type: 'array',
      description: 'entry points you checked and found SAFE, with the reason (gate present / read-only / upstream cap / validated).',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['entry_point', 'reason'],
        properties: { entry_point: { type: 'string' }, reason: { type: 'string' } },
      },
    },
  },
}

const VERDICT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['verdict', 'corrected_severity', 'rationale'],
  properties: {
    verdict: { type: 'string', enum: ['STANDS', 'REFUTED', 'DOWNGRADE'] },
    corrected_severity: { type: 'string', enum: ['info', 'low', 'medium', 'high', 'critical'] },
    rationale: { type: 'string', description: 'What you checked in real source to confirm or refute; the exact gate/reachability fact that decides it.' },
  },
}

const DOMAINS = [
  {
    key: 'wc_ajax_nopriv',
    prompt: `DOMAIN: WooCommerce UNAUTHENTICATED AJAX (the 11 nopriv events + the ?wc-ajax= frontend endpoint).
Registered in /tmp/wc-site/wp-content/plugins/woocommerce/includes/class-wc-ajax.php via
\`add_action('wp_ajax_nopriv_woocommerce_'.$e, [WC_AJAX,$e])\` AND \`add_action('wc_ajax_'.$e,...)\` for each of:
get_refreshed_fragments, apply_coupon, remove_coupon, update_shipping_method, get_cart_totals,
update_order_review, add_to_cart, remove_from_cart, checkout, get_variation, get_customer_location.
The handler method == the event name (WC_AJAX::apply_coupon, ::checkout, etc.). Also review WC_AJAX::do_wc_ajax
and how \`?wc-ajax=\` dispatches (WC_AJAX::init). For EACH handler read the real body and trace attacker input
($_POST/$_GET/$_REQUEST) to effect. Hunt: price/quantity/product-id tampering in add_to_cart/update_order_review;
coupon logic bypass in apply_coupon; unauth PII/data disclosure (get_customer_location uses geolocation of IP —
any header injection?; get_cart_totals); SQLi/param injection in any DB lookup; SSRF; the WC_AJAX::checkout path
(does it create orders unauth as intended, any missing validation enabling free/underpriced orders or order-status
abuse?). Note that these are UNAUTH by design for guest shopping — decide what is by-design vs a real defect.`,
  },
  {
    key: 'wc_ajax_priv_orders',
    prompt: `DOMAIN: WooCommerce authenticated ADMIN AJAX — ORDER/PRODUCT editing group (fires for ANY logged-in user).
In /tmp/wc-site/wp-content/plugins/woocommerce/includes/class-wc-ajax.php, events registered via
\`add_action('wp_ajax_woocommerce_'.$e,...)\` (priv only). Audit EACH of: feature_product, mark_order_status,
get_order_details, add_attribute, add_new_attribute, remove_variations, save_attributes,
add_attributes_and_variations, add_variation, link_all_variations, revoke_access_to_download,
grant_access_to_download, get_customer_details, add_order_item, add_order_fee, add_order_shipping, add_order_tax,
add_coupon_discount, remove_order_coupon, remove_order_item, remove_order_tax, calc_line_taxes, save_order_items,
load_order_items, add_order_note, delete_order_note, refund_line_items, delete_refund, load_variations,
save_variations, bulk_edit_variations, product_ordering, term_ordering, order_add_meta, order_delete_meta.
For EACH: confirm the handler enforces BOTH check_ajax_referer AND an adequate current_user_can
(edit_shop_orders / edit_products / manage_woocommerce). FLAG any handler that (a) has no capability check,
(b) checks a capability TOO LOW for the effect, or (c) checks capability only AFTER a dangerous side effect.
A 'customer' reaching any of these = privilege escalation. get_customer_details / get_order_details leaking PII to
under-privileged users is in scope.`,
  },
  {
    key: 'wc_ajax_priv_data_settings',
    prompt: `DOMAIN: WooCommerce authenticated AJAX — DATA-SEARCH + SETTINGS group (fires for ANY logged-in user).
In includes/class-wc-ajax.php: json_search_products, json_search_products_and_variations,
json_search_downloadable_products_and_variations, json_search_customers, json_search_categories,
json_search_categories_tree, json_search_taxonomy_terms, json_search_product_attributes, json_search_pages,
json_search_order_metakeys, tax_rates_save_changes, shipping_zones_save_changes, shipping_zone_add_method,
shipping_zone_remove_method, shipping_zone_methods_save_changes, shipping_zone_methods_save_settings,
shipping_classes_save_changes, shipping_providers_save_changes, toggle_gateway_enabled, update_api_key,
load_status_widget, load_recent_reviews_widget, rated. For EACH confirm nonce + adequate capability. HIGH-INTEREST:
json_search_customers (PII of all customers — what capability gates it?), get_customer_details, update_api_key
(creates/updates REST API keys → privesc/persistence if under-gated), toggle_gateway_enabled and the shipping_*
save handlers (write GLOBAL store config), tax_rates_save_changes (raw SQL — read the query; is it prepared?).
Flag missing/weak capability or any low-priv-reachable global-state write or bulk PII disclosure.`,
  },
  {
    key: 'paypal_ipn_webhooks',
    prompt: `DOMAIN: Payment webhook / IPN (UNAUTHENTICATED by design). Core WC ships the PayPal Standard gateway.
Read /tmp/wc-site/wp-content/plugins/woocommerce/includes/gateways/paypal/includes/class-wc-gateway-paypal-ipn-handler.php
and class-wc-gateway-paypal.php (the \`woocommerce_api_wc_gateway_paypal\` handler via WC_API/handle_api_requests).
Also enumerate ALL \`add_action('woocommerce_api_...\` and the WC()->api request routing (class-wc-api.php /
class-legacy-api / handle_api_requests). Threat: a forged IPN that marks an order paid/completed WITHOUT a valid
PayPal postback; receiver_email / txn amount / currency not validated; order-status transition abuse; replay;
self-mode/sandbox confusion. Read validate_ipn()/check_response() and the postback verification carefully — does the
handler REQUIRE a VERIFIED postback before mutating order state, and does it bind the IPN to the order's expected
amount/currency/receiver? Report gaps precisely (this is the classic unauth order-manipulation surface).`,
  },
  {
    key: 'rest_wc_controllers',
    prompt: `DOMAIN: WooCommerce REST API controllers — permission model + IDOR (object-level auth).
Cover the WC REST API (legacy v1/v2/v3 under includes/rest-api/Controllers, and V4 under src/Internal/RestApi).
Resolve the base permission methods get_items/get_item/create_item/update_item/delete_item/batch_items_permissions_check
and the helpers wc_rest_check_post_permissions / wc_rest_check_manager_permissions / wc_rest_check_user_permissions /
Authentication (api-key + OAuth1 scopes read/write). KEY QUESTIONS: (1) Do single-object routes (get_item/update_item/
delete_item) enforce OBJECT OWNERSHIP or just a global capability? Look specifically at orders, customers, and any
route where a shop_manager-scoped key or a customer could reach another user's object (IDOR). (2) Are there routes where
create/update permission is weaker than the effect? (3) The Authentication layer: can a read-scoped key perform writes?
Sample the highest-risk controllers (orders, customers, products, settings, webhooks, system_status, data) rather than
all — but state which you covered. Empirically confirm a couple via in-process wp-load if useful.`,
  },
  {
    key: 'rest_admin_analytics_perms',
    prompt: `DOMAIN: WC-Admin / Analytics / onboarding REST permission callbacks (the non-standard ones).
Resolve and read each of these permission methods wherever they occur (grep the tree): check_permissions,
check_permission, must_be_shop_manager_or_admin, permissions_check, snooze_task_permissions_check,
hide_task_list_permission_check, get_tasks_permission_check, get_recommended_plugins_permissions_check,
can_install_plugins, can_install_and_activate_plugins, get_product_form_permission_check, create_products_permission_check,
upload_theme_permissions_check, telemetry_permissions_check, request_catalog_permissions_check,
get_permission_check, update_current_item_permissions_check, rest_heartbeat_data_permission_check,
check_ability_permissions, my_plugin_can_analyze_text, authorize_as_authenticated, check_permission_for_fulfillments.
For EACH: what capability does it require, and is that adequate for the route's effect? FLAG any that (a) return true /
only is_user_logged_in, (b) allow a low capability (e.g. 'read', 'edit_posts') to install plugins/themes, upload,
provision, or write global config, or (c) name suggests privilege but body under-checks. Plugin/theme install or
'authorize_as_authenticated' on a privileged action are prime targets.`,
  },
  {
    key: 'rest_return_true',
    prompt: `DOMAIN: EVERY REST route with permission_callback => '__return_true' (unauthenticated). Enumerate them ALL
across /tmp/wc-site (core wp-includes + woocommerce). There are ~50 occurrences. For EACH, identify the route + handler
and classify: (SAFE) genuinely public read-only metadata / does its own auth inside / feature-flag-gated; or (RISK)
reaches a state change, data disclosure, or unvalidated action while unauthenticated. Already-analysed (report as covered,
don't re-do): WC GraphQL (wc/v4/graphql, experimental feature-flag off by default), the Agentic CheckoutSessions* routes
(Jetpack blog-token HMAC, verified fail-closed), MobileAppQRLogin exchange/scan/session-status (hardened), WC
OrderStatusRestController::get_items (static labels), jetpack connection status/remote_authorize. Focus your effort on the
REMAINING __return_true routes (core WP oembed/users/post-types collections, and any WC/vendor ones not in that list).`,
  },
  {
    key: 'jetpack_connection',
    prompt: `DOMAIN: Vendored Jetpack Connection endpoints shipped in stock WC (in scope because they register on every request).
Read src under woocommerce/vendor/automattic/jetpack-connection: the REST connector (class-rest-connector.php) routes and
their permission methods remote_register_permission_check, remote_provision_permission_check,
connection_plugins_permission_check, jetpack_register_permission_check, jetpack_reconnect_permission_check,
disconnect_site_permission_check, unlink_user_permission_callback, user_connection_data_permission_check,
identity_crisis_mitigation_permission_check; plus class-error-handler.php:927 (verify_xml_rpc_signature endpoint) and the
admin_post handlers jetpack_invite_user_to_wpcom / jetpack_resend_invite_user_to_wpcom / jetpack_revoke_invite_user_to_wpcom.
THREAT: an UNAUTH or low-priv path to remote_register / remote_provision (which can create/authorize a user or connect the
site), or to invite/provision users. Establish the real gate for each (blog-token signature? secret? nonce+cap?). Note that
some are only meant to be called by WPCOM with a secret — verify the secret/signature is actually required and fails closed.
Empirically test one (e.g. remote_register_permission_check) via in-process wp-load if feasible.`,
  },
  {
    key: 'core_wp_rest_ajax',
    prompt: `DOMAIN: WordPress CORE unauth/low-priv surface (confirm the known-safe baseline, hunt anything off).
(1) The 2 core ajax_nopriv: wp_ajax_nopriv_heartbeat, wp_ajax_nopriv_generate_password (wp-admin/includes/ajax-actions.php)
— confirm no dangerous effect. (2) Core REST controllers with __return_true or is_user_logged_in-only permission: users,
post-types, taxonomies, oembed, block-renderer, comments, search, themes — confirm object-level auth (e.g. can an
unauthenticated user enumerate users via /wp/v2/users; is that the known by-design behaviour; any field leakage?).
(3) Core low-priv ajax that write meta for OTHER users or global options (we already saw self-meta writes are fine) —
scan wp-admin/includes/ajax-actions.php for any wp_ajax_ handler missing current_user_can on a cross-user/global effect.
Report the baseline as cleared unless something genuinely deviates.`,
  },
  {
    key: 'form_handlers',
    prompt: `DOMAIN: Direct request handlers that read superglobals on init/template_redirect/admin_init/parse_request
(NOT REST/AJAX). Primary: WooCommerce WC_Form_Handler (includes/class-wc-form-handler.php) — process_login,
process_registration, save_account_details, process_lost_password (+ reset), checkout_action, pay_action, cancel_order,
order_again, save_address, add_payment_method, delete_payment_method, add_to_cart_action. For EACH: what authenticates the
actor and what authorizes the object? Focus: (a) cancel_order / pay_action rely on an ORDER KEY as a capability token —
is the key compared safely (hash_equals / not guessable / bound to order) and is order-status transition safe?
(b) save_account_details — can a user set fields they shouldn't (role, email without verification, meta)?
(c) process_registration / lost_password — user enumeration, rate-limit, token strength, account takeover.
(d) any nonce-gated global/cross-user effect. Also scan for other add_action('init'/'template_redirect', ...) handlers in WC
that consume $_GET/$_POST and act (endpoints, webhooks, download handler WC_Download_Handler). Report concrete gaps.`,
  },
]

phase('Analyze')
log(`Auditing ${DOMAINS.length} previously-uncovered entry-point domains, then adversarially verifying each finding.`)

const results = await pipeline(
  DOMAINS,
  // Stage 1: deep analysis of one domain
  (d) => agent(PREAMBLE + '\n\n' + d.prompt, {
    label: `analyze:${d.key}`,
    phase: 'Analyze',
    schema: FINDINGS_SCHEMA,
  }),
  // Stage 2: adversarially verify each finding from this domain (2 independent refuters; keep max severity that STANDS)
  (res, d) => {
    if (!res || !res.findings || res.findings.length === 0) return { domain: d.key, analysis: res, verified: [] }
    return parallel(res.findings.map((f) => () =>
      parallel([0, 1].map((i) => () =>
        agent(PREAMBLE + `

ADVERSARIAL VERIFICATION (refuter #${i + 1}). A prior auditor reported this finding. Your job is to REFUTE it by
reading the REAL source at /tmp/wc-site and, if useful, an in-process wp-load PHP check. Default to skepticism:
try hard to show the gate actually exists (upstream cap check, feature flag, nonce distribution, by-design guest
behaviour, object-ownership enforced elsewhere, input validated/prepared, effect not actually reachable). Only let it
STAND if you cannot refute it. If real but over-rated, use DOWNGRADE with the correct severity.

FINDING:
${JSON.stringify(f, null, 2)}`, {
          label: `verify:${d.key}:${(f.entry_point || f.title || '').slice(0, 24)}#${i + 1}`,
          phase: 'Verify',
          schema: VERDICT_SCHEMA,
        })
      )).then((votes) => {
        const v = votes.filter(Boolean)
        const stands = v.filter((x) => x.verdict === 'STANDS').length
        const refuted = v.filter((x) => x.verdict === 'REFUTED').length
        // survive iff not majority-refuted; record corrected severity (min of any downgrade)
        const survived = refuted < 1 && stands >= 1
        const order = ['info', 'low', 'medium', 'high', 'critical']
        let sev = f.severity
        v.forEach((x) => { if (order.indexOf(x.corrected_severity) < order.indexOf(sev)) sev = x.corrected_severity })
        return { finding: f, votes: v, survived, refuted, stands, corrected_severity: sev }
      })
    )).then((verified) => ({ domain: d.key, analysis: res, verified }))
  }
)

phase('Synthesize')
const compact = results.filter(Boolean).map((r) => ({
  domain: r.domain,
  coverage_note: r.analysis?.coverage_note,
  cleared_count: (r.analysis?.cleared || []).length,
  cleared: (r.analysis?.cleared || []).slice(0, 40),
  surviving_findings: (r.verified || []).filter((x) => x.survived).map((x) => ({
    title: x.finding.title, entry_point: x.finding.entry_point, access_class: x.finding.access_class,
    file: x.finding.file, severity: x.corrected_severity, effect: x.finding.effect,
    attack: x.finding.attack, residual_condition: x.finding.residual_condition,
    gate_present: x.finding.gate_present,
  })),
  refuted_findings: (r.verified || []).filter((x) => !x.survived).map((x) => ({
    title: x.finding.title, entry_point: x.finding.entry_point, claimed_severity: x.finding.severity,
    refuter_rationale: (x.votes.find((v) => v.verdict === 'REFUTED') || {}).rationale,
  })),
}))

const synthesis = await agent(
  PREAMBLE + `

You are the lead. Below is the machine-collected output of a fan-out auth-coverage audit across ${results.length} entry-point
domains, each finding already run through two adversarial refuters (only 'survived' findings passed).

Produce a FINAL markdown report with:
1. A COVERAGE MATRIX table: for every entry-point class (WC_AJAX nopriv, WC_AJAX priv-orders, WC_AJAX priv-data/settings,
   PayPal IPN / woocommerce_api, WC REST controllers, WC-Admin/Analytics REST perms, __return_true routes, Jetpack connection,
   core WP REST/ajax, form handlers) → count enumerated, count cleared, count surviving findings, and a one-line verdict.
   State honestly where coverage is now complete vs. sampled.
2. CONFIRMED FINDINGS (surviving, most-severe first): title, entry point, access class, file:line, gate present, effect,
   severity, concrete attack, residual condition. If there are none above 'low', say so plainly.
3. NOTABLE REFUTED candidates (what looked bad but was cleared, and the exact reason) — this is evidence of rigor.
4. A crisp bottom-line verdict on the auth/authorization posture of stock WP 7.0.2 + WC 10.9.4, and what (if anything)
   is worth reporting to the WooCommerce security team, framed honestly (severity + real-world exploitability).
Do not invent findings not in the data. Be precise and honest; cleared-with-reason is a valid outcome.

DATA:
${JSON.stringify(compact, null, 2)}`,
  { label: 'synthesize:coverage-report', phase: 'Synthesize' }
)

return { synthesis, compact }
