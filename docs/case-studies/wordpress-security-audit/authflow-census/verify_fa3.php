<?php
/*
 * F-A3 empirical verification on the real WP 7.0.2 + WC 10.9.4 install.
 * Proves the agentic-checkout auth gate (validate_jetpack_request ->
 * Rest_Authentication::is_signed_with_blog_token -> Manager::verify_xml_rpc_signature)
 * is a real HMAC gate that FAILS CLOSED:
 *   TEST 1  unauthenticated request           -> 401 / is_signed=false
 *   TEST 2  planted token, WRONG signature     -> rejected (signature_mismatch)
 *   TEST 3  planted token, CORRECT signature   -> accepted (type=blog), gate returns true
 */
error_reporting(E_ERROR | E_PARSE);
$ROUTE = '/wp-json/wc/store/v1/checkout/session';
$_SERVER['HTTPS']       = 'on';
$_SERVER['HTTP_HOST']   = 'localhost:8200';
$_SERVER['SERVER_NAME'] = 'localhost';
$_SERVER['SERVER_PORT'] = '8200';
$_SERVER['REQUEST_URI'] = $ROUTE;
$_SERVER['REQUEST_METHOD'] = 'POST';
$_GET = array();
if (!defined('JETPACK__API_VERSION')) define('JETPACK__API_VERSION', 1);
define('WP_USE_THEMES', false);
require '/tmp/wc-site/wp-load.php';

use Automattic\Jetpack\Connection\Manager;
use Automattic\Jetpack\Connection\Rest_Authentication;

$FQ_UTILS = 'Automattic\\WooCommerce\\StoreApi\\Utilities\\AgenticCheckoutUtils';
$pass = 0; $fail = 0;
function chk($cond, $msg) { global $pass,$fail; echo ($cond?"  PASS":"  FAIL")." — $msg\n"; $cond?$pass++:$fail++; return $cond; }

echo "WP ".$GLOBALS['wp_version']." / WC ".(function_exists('WC')?WC()->version:'?')."\n";
echo "class Rest_Authentication: ".(class_exists(Rest_Authentication::class)?'present (vendored)':'ABSENT')."\n";
echo "class AgenticCheckoutUtils: ".(class_exists($FQ_UTILS)?'present':'ABSENT')."\n\n";

// Helpers to drive Rest_Authentication cleanly per scenario.
function reset_auth() {
    $inst = Rest_Authentication::init();
    $r = new ReflectionObject($inst);
    foreach (['rest_authentication_status','rest_authentication_type'] as $p) {
        $pp = $r->getProperty($p); $pp->setAccessible(true); $pp->setValue($inst, ($p==='rest_authentication_status')?null:null);
    }
    // reset the memoized Manager verification too
    $cmp = $r->getProperty('connection_manager'); $cmp->setAccessible(true); $mgr = $cmp->getValue($inst);
    if ($mgr) { $rm = new ReflectionObject($mgr); if ($rm->hasProperty('xmlrpc_verification')) { $x=$rm->getProperty('xmlrpc_verification'); $x->setAccessible(true); $x->setValue($mgr,null);} }
    return $inst;
}
function run_authenticate() { $inst = reset_auth(); $inst->wp_rest_authenticate(null); return $inst; }

/* ---------------- TEST 1: unauthenticated (no signature) ---------------- */
echo "== TEST 1: unauthenticated request (no jetpack token/signature) ==\n";
$_GET = array();
$res = call_user_func([$FQ_UTILS, 'validate_jetpack_request']);
$is_err = is_wp_error($res);
$status = $is_err ? ($res->get_error_data()['status'] ?? null) : null;
chk($is_err, "validate_jetpack_request() returns WP_Error (not authorized)");
chk($status === 401, "HTTP status is 401 (got ".var_export($status,true).")");
run_authenticate();
chk(Rest_Authentication::is_signed_with_blog_token() === false, "is_signed_with_blog_token() === false when unsigned");
echo "\n";

/* ---------------- plant a blog token we control ---------------- */
$KEY = 'testkey' . substr(md5('k'), 0, 8);
$SECRET = 'S3cr3t_' . md5('blogsecret-fa3');       // arbitrary shared secret
$BLOG_TOKEN = "$KEY.$SECRET";                        // stored form: key.secret
if (!class_exists('Jetpack_Options')) { echo "Jetpack_Options ABSENT — cannot plant token\n"; }
\Jetpack_Options::update_option('blog_token', $BLOG_TOKEN);
\Jetpack_Options::update_option('id', 1234567);
$readback = \Jetpack_Options::get_option('blog_token');
echo "planted blog_token = ".($readback === $BLOG_TOKEN ? "OK ($KEY.****)" : "MISMATCH")."\n";

// Build a signed request against $ROUTE for the blog token (user_id 0).
function build_request($route, $key, $version, $good_sig, $blog_token_full) {
    $_SERVER['REQUEST_METHOD'] = 'POST';
    $_SERVER['REQUEST_URI']    = $route;
    $_GET = array(
        '_for'      => 'jetpack',
        'token'     => "$key:$version:0",     // user_id 0 => blog token
        'timestamp' => (string) time(),
        'nonce'     => substr(md5(uniqid('', true)), 0, 12),  // 12 alphanumeric chars (Jetpack limit)
    );
    // compute the *correct* signature with Jetpack's own signer + our secret
    $sig = new \Jetpack_Signature($blog_token_full, 0);
    $signature = $sig->sign_current_request(array('body' => null));
    if (is_wp_error($signature)) { echo "  [signer error] ".$signature->get_error_message()."\n"; return null; }
    $_GET['signature'] = $good_sig ? $signature : substr($signature, 0, -4) . 'AAAA'; // corrupt tail if !good
    return $signature;
}

/* ---------------- TEST 2: correct token, WRONG signature ---------------- */
echo "\n== TEST 2: valid token, TAMPERED signature ==\n";
$expected = build_request($ROUTE, $KEY, JETPACK__API_VERSION, false, $BLOG_TOKEN);
$mgr = new Manager();
$verified = $mgr->verify_xml_rpc_signature();
chk($verified === false, "Manager::verify_xml_rpc_signature() === false for wrong signature");
run_authenticate();
chk(Rest_Authentication::is_signed_with_blog_token() === false, "is_signed_with_blog_token() === false");
$res = call_user_func([$FQ_UTILS, 'validate_jetpack_request']);
chk(is_wp_error($res), "validate_jetpack_request() rejects tampered signature");

/* ---------------- TEST 3: correct token, CORRECT signature ---------------- */
echo "\n== TEST 3: valid token, CORRECT Jetpack blog-token HMAC signature ==\n";
$expected = build_request($ROUTE, $KEY, JETPACK__API_VERSION, true, $BLOG_TOKEN);
echo "  expected HMAC signature = ".substr((string)$expected,0,24)."...\n";
$mgr = new Manager();
$verified = $mgr->verify_xml_rpc_signature();
chk(is_array($verified), "Manager::verify_xml_rpc_signature() returns array (verified)");
chk(is_array($verified) && ($verified['type'] ?? null) === 'blog', "verified token type === 'blog' (got ".var_export(is_array($verified)?($verified['type']??null):$verified,true).")");
run_authenticate();
chk(Rest_Authentication::is_signed_with_blog_token() === true, "is_signed_with_blog_token() === true for correctly-signed request");
$res = call_user_func([$FQ_UTILS, 'validate_jetpack_request']);
chk($res === true, "validate_jetpack_request() === true (gate opens ONLY for a valid blog-token HMAC)");

/* ---------------- TEST 4: STOCK condition — Jetpack vendored but NOT connected ---------------- */
echo "\n== TEST 4: stock/unconnected (no blog_token stored), fully-signed request ==\n";
\Jetpack_Options::delete_option('blog_token');
\Jetpack_Options::delete_option('id');
$expected = build_request($ROUTE, $KEY, JETPACK__API_VERSION, true, $BLOG_TOKEN); // caller signs, but site has no token
$mgr = new Manager();
$verified = $mgr->verify_xml_rpc_signature();
chk($verified === false, "verify_xml_rpc_signature() === false when the site has no blog token (unconnected)");
run_authenticate(); // re-evaluate the Rest_Authentication singleton against the now-tokenless site
$res = call_user_func([$FQ_UTILS, 'validate_jetpack_request']);
chk(is_wp_error($res) && ($res->get_error_data()['status'] ?? null) === 401,
    "validate_jetpack_request() === 401 on an unconnected stock store (the real default)");

echo "\n==== RESULT: $pass passed, $fail failed ====\n";
echo "(planted test token removed; install left unconnected as before)\n";
