export const meta = {
  name: 'wc-auth-coverage-closeout',
  description: 'Close the 3 uncovered auth domains (WC_AJAX priv-orders split x2, core WP REST/ajax, form handlers), adversarially verified',
  phases: [
    { title: 'Analyze', detail: 'the previously-failed domains' },
    { title: 'Verify', detail: 'adversarial refuters per finding' },
    { title: 'Synthesize', detail: 'close-out coverage delta' },
  ],
}

const PREAMBLE = `
You are a senior application-security auditor doing a WHITEBOX audit of a REAL, RUNNING install:
- Source tree: /tmp/wc-site  (WordPress 7.0.2 core + WooCommerce 10.9.4 plugin). READ real code here.
- Live install bootable IN-PROCESS: a short PHP script that does \`$_SERVER[...]; define('WP_USE_THEMES',false); require '/tmp/wc-site/wp-load.php';\` then calls functions, run with \`php script.php\`. MariaDB up (wpc/wpc@127.0.0.1, siteurl http://localhost:8200). Do NOT run a long-lived \`php -S\` (killed by sandbox). If you write options, delete them after.
- Elasticsearch-compatible indexes on http://127.0.0.1:9200: \`wp-wc-authflow\`, \`wp-wc-methods\`.

THREAT MODEL: Unauthenticated attacker; ALSO any self-registered 'customer'/'subscriber' is hostile (WooCommerce allows open registration), so "any logged-in user" ≈ attacker. A NONCE is CSRF protection, NOT authorization (per-user/action/session; distribution matters). Hunt: MISSING capability on a privileged/cross-user/global effect; OBJECT-LEVEL/IDOR gaps (checks logged-in but not ownership); UNAUTH reaching a dangerous effect (state change, PII disclosure, SQLi, SSRF, file write/delete, RCE, order/price/coupon tampering, auth bypass, account takeover); capability too LOW for the effect.

DISCIPLINE: This codebase is well-hardened; be precise, not credulous. Follow the call chain — capability checks are often UPSTREAM (action dispatcher, base controller, feature flag, admin-page load, map_meta_cap). Empirically confirm privileged caps for a 'customer' via in-process wp-load when in doubt. For every candidate: concrete attacker request, exact missing/weak gate, effect, severity, residual_condition. A cleared item WITH the reason it's safe is a valuable result — put those in \`cleared\`.
OUTPUT SIZE: keep \`cleared\` to the ~20 most representative entries; if you covered more, summarize the remainder in \`coverage_note\` with counts. Keep every string field concise. Do not exceed a few findings unless genuinely warranted.
`

const FINDINGS_SCHEMA = {
  type: 'object', additionalProperties: false,
  required: ['domain', 'coverage_note', 'findings', 'cleared'],
  properties: {
    domain: { type: 'string' },
    coverage_note: { type: 'string' },
    findings: {
      type: 'array',
      items: {
        type: 'object', additionalProperties: false,
        required: ['title', 'entry_point', 'access_class', 'file', 'handler', 'gate_present', 'effect', 'severity', 'attack', 'residual_condition'],
        properties: {
          title: { type: 'string' }, entry_point: { type: 'string' },
          access_class: { type: 'string', enum: ['unauthenticated', 'any_logged_in', 'low_priv_role', 'nonce_only', 'other'] },
          file: { type: 'string' }, handler: { type: 'string' }, gate_present: { type: 'string' },
          effect: { type: 'string' }, severity: { type: 'string', enum: ['info', 'low', 'medium', 'high', 'critical'] },
          attack: { type: 'string' }, residual_condition: { type: 'string' },
        },
      },
    },
    cleared: {
      type: 'array',
      items: { type: 'object', additionalProperties: false, required: ['entry_point', 'reason'], properties: { entry_point: { type: 'string' }, reason: { type: 'string' } } },
    },
  },
}

const VERDICT_SCHEMA = {
  type: 'object', additionalProperties: false,
  required: ['verdict', 'corrected_severity', 'rationale'],
  properties: {
    verdict: { type: 'string', enum: ['STANDS', 'REFUTED', 'DOWNGRADE'] },
    corrected_severity: { type: 'string', enum: ['info', 'low', 'medium', 'high', 'critical'] },
    rationale: { type: 'string' },
  },
}

const DOMAINS = [
  {
    key: 'wc_ajax_priv_orders_A',
    prompt: `DOMAIN: WooCommerce authenticated ADMIN AJAX — ORDER-EDITING subgroup (fires for ANY logged-in user).
File: /tmp/wc-site/wp-content/plugins/woocommerce/includes/class-wc-ajax.php. Handler method == event name.
Audit EACH of ONLY these: mark_order_status, get_order_details, add_order_item, add_order_fee, add_order_shipping,
add_order_tax, add_coupon_discount, remove_order_coupon, remove_order_item, remove_order_tax, calc_line_taxes,
save_order_items, load_order_items, add_order_note, delete_order_note, refund_line_items, delete_refund,
order_add_meta, order_delete_meta, get_customer_details.
For EACH: confirm the handler enforces BOTH check_ajax_referer AND an adequate current_user_can (expected
'edit_shop_orders'). FLAG any handler with no capability check, a capability too LOW for the effect, or a check that
runs AFTER a dangerous side effect. A 'customer' reaching any of these = privilege escalation / order tampering /
PII disclosure (get_customer_details, get_order_details). Empirically confirm a customer lacks 'edit_shop_orders'
if you flag anything. Put cleared handlers (with the exact cap they enforce) in \`cleared\`.`,
  },
  {
    key: 'wc_ajax_priv_orders_B',
    prompt: `DOMAIN: WooCommerce authenticated ADMIN AJAX — PRODUCT/VARIATION/DOWNLOAD subgroup (fires for ANY logged-in user).
File: /tmp/wc-site/wp-content/plugins/woocommerce/includes/class-wc-ajax.php. Handler method == event name.
Audit EACH of ONLY these: feature_product, add_attribute, add_new_attribute, remove_variations, save_attributes,
add_attributes_and_variations, add_variation, link_all_variations, load_variations, save_variations,
bulk_edit_variations, product_ordering, term_ordering, revoke_access_to_download, grant_access_to_download.
For EACH: confirm BOTH check_ajax_referer AND an adequate current_user_can (expected 'edit_products', and
'edit_shop_orders' for the download grant/revoke which touch order permissions). FLAG missing/too-low capability or
check-after-effect. grant_access_to_download / revoke_access_to_download are highest-interest (they mutate download
permissions tied to orders). Empirically confirm a customer lacks 'edit_products'/'edit_shop_orders' if you flag
anything. Put cleared handlers (with the cap they enforce) in \`cleared\`.`,
  },
  {
    key: 'core_wp_rest_ajax',
    prompt: `DOMAIN: WordPress CORE unauth/low-priv surface (confirm the known-safe baseline; hunt anything off).
(1) The 2 core ajax_nopriv (wp-admin/includes/ajax-actions.php): wp_ajax_nopriv_heartbeat, wp_ajax_nopriv_generate_password
— confirm no dangerous effect. (2) Core REST controllers with __return_true or is_user_logged_in-only permission:
/wp/v2/users (enumeration — by-design? field leakage?), types, taxonomies, oembed, block-renderer, comments, search,
settings, themes, plugins, block-directory. For single-object write/read, confirm object-level auth. (3) Scan
wp-admin/includes/ajax-actions.php for ANY wp_ajax_ handler missing current_user_can on a CROSS-USER or GLOBAL effect
(self-meta writes are fine). Report the baseline as cleared unless something genuinely deviates. Keep cleared concise.`,
  },
  {
    key: 'form_handlers',
    prompt: `DOMAIN: Direct request handlers reading superglobals on init/template_redirect/wp_loaded/admin_post (NOT REST/AJAX).
Primary: WooCommerce WC_Form_Handler (/tmp/wc-site/wp-content/plugins/woocommerce/includes/class-wc-form-handler.php).
Audit EACH: process_login, process_registration, save_account_details, process_lost_password + reset-password flow,
checkout_action, pay_action, cancel_order, order_again, save_address, add_payment_method, delete_payment_method,
add_to_cart_action. For EACH: what AUTHENTICATES the actor and what AUTHORIZES the object?
Key hunts: (a) cancel_order / pay_action use an ORDER KEY capability token — is the key compared safely (hash_equals,
unguessable, bound to the order) and is the status transition safe (can a guest cancel/pay someone else's order)?
(b) save_account_details — can a user set fields they shouldn't (role, email without verification, arbitrary meta,
password of another)? (c) process_registration / lost_password — user enumeration, token strength, account takeover,
rate-limit. (d) any nonce-gated GLOBAL/cross-user effect. Also grep WC for other add_action('init'|'template_redirect'|
'wp_loaded', ...) handlers consuming $_GET/$_POST and acting — especially WC_Download_Handler (download_product) and
any endpoint/webhook listener. Report concrete gaps; cleared items with the gate in \`cleared\`.`,
  },
]

phase('Analyze')
log(`Closing the 3 previously-failed domains (priv-orders split x2, core WP, form handlers).`)

const results = await pipeline(
  DOMAINS,
  (d) => agent(PREAMBLE + '\n\n' + d.prompt, { label: `analyze:${d.key}`, phase: 'Analyze', schema: FINDINGS_SCHEMA }),
  (res, d) => {
    if (!res || !res.findings || res.findings.length === 0) return { domain: d.key, analysis: res, verified: [] }
    return parallel(res.findings.map((f) => () =>
      parallel([0, 1].map((i) => () =>
        agent(PREAMBLE + `

ADVERSARIAL VERIFICATION (refuter #${i + 1}). A prior auditor reported this finding. REFUTE it by reading REAL source
at /tmp/wc-site and, if useful, an in-process wp-load PHP check. Default to skepticism: show the gate exists (upstream
cap, map_meta_cap resolution, feature flag, nonce distribution, by-design behaviour, object-ownership, validated input,
unreachable). STAND only if you cannot refute. If real but over-rated, DOWNGRADE with the correct severity.

FINDING:
${JSON.stringify(f, null, 2)}`, { label: `verify:${d.key}:${(f.entry_point || f.title || '').slice(0, 22)}#${i + 1}`, phase: 'Verify', schema: VERDICT_SCHEMA })
      )).then((votes) => {
        const v = votes.filter(Boolean)
        const stands = v.filter((x) => x.verdict === 'STANDS').length
        const refuted = v.filter((x) => x.verdict === 'REFUTED').length
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
  cleared: (r.analysis?.cleared || []).slice(0, 30),
  surviving_findings: (r.verified || []).filter((x) => x.survived).map((x) => ({
    title: x.finding.title, entry_point: x.finding.entry_point, access_class: x.finding.access_class,
    file: x.finding.file, severity: x.corrected_severity, effect: x.finding.effect,
    attack: x.finding.attack, residual_condition: x.finding.residual_condition, gate_present: x.finding.gate_present,
  })),
  refuted_findings: (r.verified || []).filter((x) => !x.survived).map((x) => ({
    title: x.finding.title, entry_point: x.finding.entry_point, claimed_severity: x.finding.severity,
    refuter_rationale: (x.votes.find((v) => v.verdict === 'REFUTED') || {}).rationale,
  })),
}))

const synthesis = await agent(
  PREAMBLE + `

You are the lead, closing out the 3 domains that failed in the prior audit batch. Below is the machine-collected,
adversarially-verified output for: WC_AJAX priv-orders (editing subgroup A + product/download subgroup B), core WP
REST/ajax, and WC form handlers. Produce a concise markdown CLOSE-OUT with:
1. A coverage table for these 4 domain slices: enumerated / cleared / surviving, one-line verdict, and whether coverage
   is now COMPLETE.
2. CONFIRMED FINDINGS (surviving, most-severe first) with entry point, access class, file:line, gate, effect, severity,
   concrete attack, residual condition. If none above 'low', say so plainly.
3. NOTABLE REFUTED candidates and the exact reason each was cleared.
4. One-paragraph verdict on whether these three surfaces change the overall posture conclusion (prior batch: no finding
   above info; strong, internally-consistent authorization). Be precise and honest; do not invent findings.

DATA:
${JSON.stringify(compact, null, 2)}`,
  { label: 'synthesize:closeout', phase: 'Synthesize' }
)

return { synthesis, compact }
