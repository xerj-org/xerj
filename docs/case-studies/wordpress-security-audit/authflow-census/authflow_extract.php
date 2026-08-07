<?php
/*
 * Auth-flow feature extractor for XERJ.
 * Emits NDJSON (ES bulk) for two doc kinds into index `wp-wc-authflow`:
 *   kind=entry  — an entry-point registration (REST route / AJAX / admin-post)
 *   kind=method — a function/method body, tagged with auth-checks, dangerous sinks, superglobal reads, calls[]
 * Models the UNAUTHENTICATED / UNPROTECTED surface (capability + nonce gates vs. dangerous effects),
 * which the deserialization index (wp-wc-methods) never captured.
 *
 * NOTE (coverage limit, documented): the entry-point regex matches only LITERAL action strings.
 * WooCommerce registers most AJAX via string concatenation in loops (add_action('wp_ajax_woocommerce_'.$e,...))
 * and payment webhooks via woocommerce_api_*; those were enumerated separately by hand + agents (see AUTHFLOW-FINDINGS.md).
 * Usage: php authflow_extract.php /tmp/wc-site /tmp/wcom-authflow/authflow.ndjson
 */
error_reporting(E_ALL & ~E_DEPRECATED);
$root = $argv[1] ?? '/tmp/wc-site';
$out  = $argv[2] ?? '/tmp/wcom-authflow/authflow.ndjson';

$files = [];
$it = new RecursiveIteratorIterator(new RecursiveDirectoryIterator($root, FilesystemIterator::SKIP_DOTS));
foreach ($it as $f) if ($f->isFile() && substr($f->getFilename(), -4) === '.php') $files[] = $f->getPathname();
sort($files);

function auth_checks($b) {
    $a = [];
    foreach ([
        'current_user_can' => '/\bcurrent_user_can\s*\(/', 'user_can' => '/\buser_can\s*\(/',
        'check_ajax_referer' => '/\bcheck_ajax_referer\s*\(/', 'check_admin_referer' => '/\bcheck_admin_referer\s*\(/',
        'wp_verify_nonce' => '/\bwp_verify_nonce\s*\(/', 'is_user_logged_in' => '/\bis_user_logged_in\s*\(/',
        'wp_get_current_user' => '/\bwp_get_current_user\s*\(/',
    ] as $k => $re) if (preg_match($re, $b)) $a[] = $k;
    if (preg_match('/\b(current_user_can|user_can)\b.*\b(manage_woocommerce|manage_options|edit_)/', $b)) $a[] = 'cap_admin_level';
    return array_values(array_unique($a));
}

function sinks($b) {
    $s = [];
    if (preg_match('/\$wpdb\s*->\s*(query|get_results|get_row|get_var|get_col)\s*\(/', $b)) $s[] = 'wpdb_read_or_query';
    if (preg_match('/\$wpdb\s*->\s*(query|get_results|get_row|get_var|get_col)\s*\(\s*["\'][^"\']*\$/', $b) ||
        preg_match('/\$wpdb\s*->\s*(query|get_results|get_row|get_var|get_col)\s*\(\s*\$\w+/', $b)) $s[] = 'sql_interp_candidate';
    if (preg_match('/\$wpdb\s*->\s*prepare\s*\(/', $b)) $s[] = 'wpdb_prepare';
    if (preg_match('/\b(file_put_contents|fwrite|fputs)\s*\(/', $b)) $s[] = 'file_write';
    if (preg_match('/\b(unlink|wp_delete_file|rmdir)\s*\(/', $b)) $s[] = 'file_delete';
    if (preg_match('/\b(move_uploaded_file|wp_handle_upload|wp_handle_sideload)\s*\(/', $b)) $s[] = 'file_upload';
    if (preg_match('/\b(include|require)(_once)?\b[^;]*\$/', $b)) $s[] = 'include_var';
    if (preg_match('/\b(system|exec|shell_exec|passthru|popen|proc_open)\s*\(/', $b)) $s[] = 'exec';
    if (preg_match('/\beval\s*\(|\bcreate_function\s*\(|\bassert\s*\(\s*\$/', $b)) $s[] = 'eval';
    if (preg_match('/\bcall_user_func(_array)?\s*\(\s*\$/', $b) || preg_match('/\(\s*\$\w+\s*\)\s*\(/', $b)) $s[] = 'dynamic_call';
    if (preg_match('/\b(update_option|add_option)\s*\(/', $b)) $s[] = 'option_write';
    if (preg_match('/\b(update_user_meta|add_user_meta|delete_user_meta)\s*\(/', $b)) $s[] = 'user_meta_write';
    if (preg_match('/\b(wp_insert_user|wp_update_user|wp_create_user)\s*\(/', $b)) $s[] = 'user_create_update';
    if (preg_match('/->\s*set_role\s*\(|->\s*add_role\s*\(|->\s*add_cap\s*\(/', $b)) $s[] = 'role_write';
    if (preg_match('/\b(wp_set_auth_cookie|wp_set_current_user|wp_signon|wp_set_password)\s*\(/', $b)) $s[] = 'auth_state';
    if (preg_match('/\b(add_user_to_blog|grant_super_admin)\s*\(/', $b)) $s[] = 'blog_role';
    if (preg_match('/\b(maybe_unserialize|unserialize)\s*\(/', $b)) $s[] = 'unserialize';
    if (preg_match('/\bwp_remote_(get|post|request|head)\s*\(\s*\$/', $b)) $s[] = 'remote_fetch_var';
    if (preg_match('/\bwp_redirect\s*\(/', $b)) $s[] = 'redirect_open';
    if (preg_match('/\becho\b[^;]*\$_(GET|POST|REQUEST|COOKIE|SERVER)/', $b)) $s[] = 'echo_super';
    if (preg_match('/\bupdate_post_meta\s*\(|\bdelete_post_meta\s*\(/', $b)) $s[] = 'post_meta_write';
    return array_values(array_unique($s));
}

function reads_super($b) {
    $r = [];
    foreach (['_GET','_POST','_REQUEST','_COOKIE','_FILES','_SERVER'] as $g) if (strpos($b, '$'.$g) !== false) $r[] = $g;
    if (preg_match('#php://input#', $b)) $r[] = 'php_input';
    if (preg_match('/\bget_json_params\s*\(|\bget_param\s*\(|\bget_body\s*\(/', $b)) $r[] = 'rest_param';
    return array_values(array_unique($r));
}

function calls($b) {
    $c = [];
    if (preg_match_all('/(?<![\w$>])([a-zA-Z_]\w{2,})\s*\(/', $b, $m)) $c = array_merge($c, $m[1]);
    if (preg_match_all('/->\s*([a-zA-Z_]\w{2,})\s*\(/', $b, $m)) $c = array_merge($c, array_map(fn($x)=>'->'.$x, $m[1]));
    $skip = array_flip(['if','for','foreach','while','switch','array','isset','empty','list','return','echo','print','unset','sprintf','esc_html','esc_attr','esc_url','__','_e','_x','sanitize_text_field','absint','intval','count','is_array','is_string','in_array','sizeof']);
    return array_slice(array_values(array_filter(array_unique($c), fn($x)=> !isset($skip[ltrim($x,'>-')]))), 0, 40);
}

function body_at($toks, $i, $N) {
    $pos = null;
    for ($k = $i; $k < $N; $k++) { if ($toks[$k] === '{') { $pos = $k; break; } if ($toks[$k] === ';') return null; }
    if ($pos === null) return null;
    $d = 0; $txt = '';
    for ($k = $pos; $k < $N; $k++) { $s = is_array($toks[$k]) ? $toks[$k][1] : $toks[$k]; $txt .= $s; if ($s === '{') $d++; elseif ($s === '}') { $d--; if (!$d) break; } }
    return $txt;
}

$fh = fopen($out, 'w'); $nMethod = 0; $nEntry = 0;
$ENTRY_RE = '/\b(register_rest_route|add_action|add_filter)\s*\(/';
foreach ($files as $file) {
    $src = @file_get_contents($file); if ($src === false) continue;
    $rel = str_replace($root.'/', '', $file);
    $hasFn = strpos($src, 'function') !== false; $hasReg = preg_match($ENTRY_RE, $src);
    if (!$hasFn && !$hasReg) continue;
    $toks = @token_get_all($src); if (!$toks) continue; $N = count($toks);
    if ($hasReg) {
        if (preg_match_all('/register_rest_route\s*\(/', $src, $mm, PREG_OFFSET_CAPTURE)) {
            foreach ($mm[0] as $mo) {
                $off = $mo[1]; $chunk = substr($src, $off, 1200); $ln = substr_count($src, "\n", 0, $off) + 1;
                $perm = null; $cb = null; $methods = null;
                if (preg_match('/[\'"]permission_callback[\'"]\s*=>\s*([^,\n]+)/', $chunk, $p)) $perm = trim($p[1]);
                if (preg_match('/[\'"]callback[\'"]\s*=>\s*([^,\n]+)/', $chunk, $c)) $cb = trim($c[1]);
                if (preg_match('/[\'"]methods[\'"]\s*=>\s*([^,\n]+)/', $chunk, $me)) $methods = trim($me[1]);
                $unauth = ($perm === null) ? 1 : (preg_match('/__return_true|__return_empty|"?true"?\s*$/i', $perm) ? 1 : 0);
                $doc = ['kind'=>'entry','entry_type'=>'rest','file'=>$rel,'line'=>$ln,'permission_callback'=>$perm,'callback_ref'=>$cb,'methods'=>$methods,'unauth'=>$unauth,'snippet'=>substr(preg_replace('/\s+/', ' ', $chunk), 0, 240)];
                fwrite($fh, json_encode(['index'=>['_index'=>'wp-wc-authflow']])."\n".json_encode($doc)."\n"); $nEntry++;
            }
        }
        if (preg_match_all('/add_action\s*\(\s*[\'"](wp_ajax_nopriv_|wp_ajax_|admin_post_nopriv_|admin_post_)([a-zA-Z0-9_\-]+)[\'"]\s*,\s*([^,\)]+)/', $src, $mm, PREG_OFFSET_CAPTURE|PREG_SET_ORDER)) {
            foreach ($mm as $m) {
                $off = $m[0][1]; $ln = substr_count($src, "\n", 0, $off) + 1;
                $hook = rtrim($m[1][0], '_'); $action = $m[2][0]; $cb = trim($m[3][0]);
                $type = strpos($hook,'nopriv')!==false ? (strpos($hook,'ajax')!==false?'ajax_nopriv':'adminpost_nopriv') : (strpos($hook,'ajax')!==false?'ajax_auth':'adminpost_auth');
                $unauth = strpos($type,'nopriv')!==false ? 1 : 0;
                $doc = ['kind'=>'entry','entry_type'=>$type,'file'=>$rel,'line'=>$ln,'action'=>$action,'callback_ref'=>$cb,'unauth'=>$unauth,'snippet'=>substr(preg_replace('/\s+/', ' ', substr($src,$off,160)),0,160)];
                fwrite($fh, json_encode(['index'=>['_index'=>'wp-wc-authflow']])."\n".json_encode($doc)."\n"); $nEntry++;
            }
        }
    }
    if (!$hasFn) continue;
    $class = null;
    for ($i = 0; $i < $N; $i++) {
        $t = $toks[$i];
        if (is_array($t) && in_array($t[0], [T_CLASS, T_INTERFACE, T_TRAIT], true)) {
            for ($j = $i+1; $j < $N; $j++) { if (is_array($toks[$j]) && $toks[$j][0] === T_STRING) { $class = $toks[$j][1]; break; } if ($toks[$j] === '{') break; }
        }
        if (is_array($t) && $t[0] === T_FUNCTION) {
            $mn = null; $ml = $t[2];
            for ($j = $i+1; $j < $N; $j++) { if (is_array($toks[$j]) && $toks[$j][0] === T_STRING) { $mn = $toks[$j][1]; break; } if ($toks[$j] === '(') break; }
            if (!$mn) continue;
            $txt = body_at($toks, $i, $N); if ($txt === null) continue;
            $ac = auth_checks($txt); $sk = sinks($txt); $rs = reads_super($txt); $cl = calls($txt);
            $danger = array_values(array_intersect($sk, ['sql_interp_candidate','file_write','file_delete','file_upload','include_var','exec','eval','dynamic_call','option_write','user_meta_write','user_create_update','role_write','auth_state','blog_role','unserialize','remote_fetch_var']));
            $doc = ['kind'=>'method','class'=>$class,'method'=>$mn,'file'=>$rel,'line'=>$ml,'auth_checks'=>$ac,'has_auth'=>$ac?1:0,'sinks'=>$sk,'danger_sinks'=>$danger,'has_danger'=>$danger?1:0,'reads_super'=>$rs,'reads_input'=>$rs?1:0,'calls'=>$cl,'body_len'=>strlen($txt)];
            fwrite($fh, json_encode(['index'=>['_index'=>'wp-wc-authflow']])."\n".json_encode($doc)."\n"); $nMethod++;
        }
    }
}
fclose($fh);
echo "entries=$nEntry methods=$nMethod -> $out\n";
