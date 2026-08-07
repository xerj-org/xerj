Confirmed both customer and subscriber lack every subgroup-B capability, and cleanup left 0 residual users. Subgroup B is now fully closed by direct audit. Here is the close-out.

---

# CLOSE-OUT — WC_AJAX priv-orders (A+B), core WP REST/ajax, WC form handlers

**Scope note:** The provided batch contained data for 3 domains (`wc_ajax_priv_orders_A`, `core_wp_rest_ajax`, `form_handlers`). The 4th named slice — WC_AJAX **product/download subgroup B** — had **no data entry**. I closed it myself by direct whitebox audit of all 38 remaining privileged handlers in `class-wc-ajax.php`, including in-process wp-load cap confirmation. Results below.

## 1. Coverage table

| Domain slice | Enumerated | Cleared | Surviving | Verdict | Complete? |
|---|---:|---:|---:|---|:--:|
| WC_AJAX priv-orders **A** (order editing) | 20 | 20 | 0 | Every handler enforces nonce **AND** `edit_shop_orders`(+) before any effect; no per-user ownership model to bypass | ✅ |
| WC_AJAX priv-orders **B** (product/download/variation/attr/search/shipping/tax/gateway) | 38 | 38 | 0 | All gated by `edit_products`/`manage_product_terms`/`edit_shop_orders`/`manage_woocommerce`(+); `json_search_products*` cap-less but nonce+`read_product` public-only filtered | ✅ |
| core WP REST/ajax | ~143 (2 nopriv + 94 `wp_ajax_` + 47 REST controllers) | 15 repr. of ~40 detailed | 0 | Hardened upstream baseline intact; the one WC deviation (`note` comment_type) gated end-to-end | ✅ |
| WC form handlers | 21 (17 `WC_Form_Handler` + download + meta-cap map + IPN + admin save) | 16 repr. | 0 | AuthN + nonce/order-key/ownership all correct; no mass-assignment, no IDOR | ✅ |

**All four slices are now COMPLETE.**

## 2. Confirmed findings (surviving)

**None above `info`.** Zero findings of severity `low` or higher survived verification across all four slices. Subgroup B produced no missing-capability, IDOR, or unauth-reachable-effect. Subgroup A, core WP, and form handlers each returned zero surviving findings in the collected data, confirmed consistent with the tree.

## 3. Notable refuted / cleared candidates (with reason)

- **`json_search_products` / `json_search_products_and_variations` / `json_search_downloadable_products_and_variations`** (`class-wc-ajax.php:1788,1886,1897`) — *no `current_user_can`*, which looks like a missing-cap on a disclosure endpoint. **Cleared:** registered priv-only (in `$ajax_events`, no `nopriv`) so unauth cannot reach it; requires the per-user `search-products` nonce (no frontend distributes it to customers); and each result is filtered by `wc_products_array_filter_readable` → `read_product`, which resolves to public products only (already visible in the shop). Worst case leaks nothing non-public. Matches upstream.
- **`json_search_order_metakeys` → `CustomMetaBox::search_metakeys_ajax`** (`CustomMetaBox.php:216`) — delegated handler that showed neither nonce nor cap in the top-level file. **Cleared:** enforces `check_ajax_referer('search-order-metakeys')` + `current_user_can('edit_shop_orders')` + order-exists (lines 217-227) before returning read-only meta-key autofill. Customer lacks the cap.
- **Shipping/tax/gateway settings savers** (`tax_rates_save_changes`, `shipping_zone*`, `shipping_classes/providers_save_changes`, `toggle_gateway_enabled`) — `check_ajax_referer` not always at the top. **Cleared:** each opens with `current_user_can('manage_woocommerce')` → `wp_die(-1)` (lines 3246/3325/3415/3487/3545/3716/3770/3902/4123) before any write; capability alone excludes customer/subscriber regardless of nonce placement.
- **`rated`** (`:2480`) — nonce-less option write. **Cleared:** `manage_woocommerce`-gated (line 2481); only sets an admin footer flag. Benign, admin-only.
- **`update_api_key`** (`:2495`) — REST API key issuance. **Cleared:** nonce + `manage_woocommerce` + `edit_user` on any target user id (line 2525). No cross-user key minting.
- **Form-handler account enumeration / reset-email flooding** (lost-password/registration/login; already in batch as refuted, `low`). **Cleared/refuted:** distinct errors + no rate limit are the documented upstream WP/WC behavior; informational, not a WC-introduced authorization defect.

**Empirical basis:** in-process wp-load confirmed both `customer` and `subscriber` hold `edit_products=0, manage_product_terms=0, edit_shop_orders=0, edit_others_shop_orders=0, manage_woocommerce=0, publish_shop_orders=0, view_woocommerce_reports=0, read_private_products=0, edit_others_products=0`. Throwaway test users deleted (0 residual).

## 4. Verdict on overall posture

These three surfaces (four slices) **do not change** the prior batch's posture conclusion. Across ~220 enumerated entry points, every privileged, cross-user, or global effect is gated — upstream in the dispatcher (priv-only registration), in-handler (nonce + capability short-circuit before side effects), or in a followed delegate (`CustomMetaBox`, controllers) — and the two apparent cap-less outliers (`json_search_products*`) are constrained to public data by a `read_product` filter. Capabilities are also correctly *stratified*: read/disclosure on `edit_shop_orders`/`edit_products`, mutation of global settings on `manage_woocommerce`, meta-mutation on the stronger `manage_woocommerce ∧ edit_others_shop_orders`. The authorization model is strong and internally consistent; the sole surviving items are `info`-level upstream behaviors. Overall conclusion stands: **no finding above info; well-hardened, coherent authorization.**