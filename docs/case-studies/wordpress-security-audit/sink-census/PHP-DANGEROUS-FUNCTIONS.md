# The PHP dangerous-function map (full reference)

287 functions across 30 categories.

## command-exec — RCE-command
| function | kind | arg | note |
|---|---|---|---|
| `exec` | call | 0 |  |
| `shell_exec` | call | 0 |  |
| `system` | call | 0 |  |
| `passthru` | call | 0 |  |
| `proc_open` | call | 0 |  |
| `popen` | call | 0 |  |
| `pcntl_exec` | call | 0 |  |
| `expect_popen` | call | 0 |  |
| `ssh2_exec` | call | 1 |  |
| `ssh2_shell` | call | 0 |  |
| `dl` | call | 0 | loads a native extension |
| `w32api_invoke_function` | call | 0 |  |

## backtick-operator — RCE-command
| function | kind | arg | note |
|---|---|---|---|
| `shell_command_expression` | construct | self | `...` backtick executes a shell command |

## code-eval — RCE-code
| function | kind | arg | note |
|---|---|---|---|
| `eval` | call | 0 |  |
| `assert` | call | 0 | string arg eval'd pre-8.0 |
| `create_function` | call | 1 | eval-backed; removed 8.0 |
| `preg_replace` | call | 0 | /e modifier eval's replacement (removed 7.0); user pattern=ReDoS |
| `mb_ereg_replace` | call | 3 | 'e' option evals |
| `mb_eregi_replace` | call | 3 | 'e' option evals |
| `ReflectionFunction` | call | 0 | ->invoke() runs an arbitrary function |

## dynamic-callable — RCE-code-callable
*arbitrary function/method call if the callable is attacker-controlled (second-order)*

| function | kind | arg | note |
|---|---|---|---|
| `call_user_func` | call | cb:0 |  |
| `call_user_func_array` | call | cb:0 |  |
| `call_user_method` | call | cb:0 |  |
| `call_user_method_array` | call | cb:0 |  |
| `forward_static_call` | call | cb:0 |  |
| `forward_static_call_array` | call | cb:0 |  |
| `array_map` | call | cb:0 |  |
| `array_filter` | call | cb:1 |  |
| `array_walk` | call | cb:1 |  |
| `array_walk_recursive` | call | cb:1 |  |
| `array_reduce` | call | cb:1 |  |
| `array_diff_uassoc` | call | cb:2 |  |
| `usort` | call | cb:1 |  |
| `uasort` | call | cb:1 |  |
| `uksort` | call | cb:1 |  |
| `preg_replace_callback` | call | cb:1 |  |
| `preg_replace_callback_array` | call | cb:0 |  |
| `register_shutdown_function` | call | cb:0 |  |
| `register_tick_function` | call | cb:0 |  |
| `set_error_handler` | call | cb:0 |  |
| `set_exception_handler` | call | cb:0 |  |
| `spl_autoload_register` | call | cb:0 |  |
| `ob_start` | call | cb:0 |  |
| `iterator_apply` | call | cb:1 |  |
| `filter_var` | call | 2 | FILTER_CALLBACK option runs a callable |
| `header_register_callback` | call | cb:0 |  |
| `stream_filter_register` | call | 1 |  |

## deserialization — deserialization
*PHP Object Injection via magic methods (POP chains)*

| function | kind | arg | note |
|---|---|---|---|
| `unserialize` | call | 0 |  |
| `yaml_parse` | call | 0 |  |
| `yaml_parse_file` | call | 0 |  |
| `yaml_parse_url` | call | 0 |  |
| `igbinary_unserialize` | call | 0 |  |
| `msgpack_unserialize` | call | 0 |  |
| `wddx_deserialize` | call | 0 |  |
| `session_decode` | call | 0 | deserializes into $_SESSION |

## file-include — LFI-RFI
*includes AND executes the path; RFI if allow_url_include*

| function | kind | arg | note |
|---|---|---|---|
| `include` | construct | self |  |
| `include_once` | construct | self |  |
| `require` | construct | self |  |
| `require_once` | construct | self |  |
| `virtual` | call | 0 | Apache subrequest |

## file-read — file-read
*arbitrary file read / path traversal; several also accept URL wrappers (SSRF)*

| function | kind | arg | note |
|---|---|---|---|
| `file_get_contents` | call | 0 | URL => SSRF; php://filter => source disclosure |
| `file` | call | 0 |  |
| `fopen` | call | 0 | URL wrappers => SSRF |
| `readfile` | call | 0 |  |
| `fpassthru` | call | 0 |  |
| `fgets` | call | 0 |  |
| `fgetc` | call | 0 |  |
| `fread` | call | 0 |  |
| `fscanf` | call | 0 |  |
| `parse_ini_file` | call | 0 |  |
| `parse_ini_string` | call | 0 |  |
| `highlight_file` | call | 0 |  |
| `show_source` | call | 0 |  |
| `readlink` | call | 0 |  |
| `realpath` | call | 0 |  |
| `gzfile` | call | 0 |  |
| `gzopen` | call | 0 |  |
| `readgzfile` | call | 0 |  |
| `bzopen` | call | 0 |  |
| `bzread` | call | 0 |  |
| `zip_open` | call | 0 |  |
| `exif_read_data` | call | 0 |  |
| `exif_thumbnail` | call | 0 |  |
| `getimagesize` | call | 0 | URL => SSRF |
| `getimagesizefromstring` | call | 0 |  |
| `finfo_file` | call | 1 |  |
| `mime_content_type` | call | 0 |  |
| `imagecreatefromjpeg` | call | 0 | URL => SSRF |
| `imagecreatefrompng` | call | 0 |  |
| `imagecreatefromgif` | call | 0 |  |
| `imagecreatefromwebp` | call | 0 |  |
| `SplFileObject` | method | 0 |  |
| `md5_file` | call | 0 |  |
| `sha1_file` | call | 0 |  |
| `hash_file` | call | 1 |  |
| `stat` | call | 0 |  |
| `lstat` | call | 0 |  |
| `fileperms` | call | 0 |  |

## file-write — file-write
*write/overwrite; RCE if attacker controls path+content of a .php*

| function | kind | arg | note |
|---|---|---|---|
| `file_put_contents` | call | 0 |  |
| `fwrite` | call | 0 | handle arg |
| `fputs` | call | 0 |  |
| `fputcsv` | call | 0 |  |
| `move_uploaded_file` | call | 1 |  |
| `copy` | call | 1 | URL src => SSRF; dest => write |
| `rename` | call | 1 |  |
| `link` | call | 1 |  |
| `symlink` | call | 1 |  |
| `touch` | call | 0 |  |
| `mkdir` | call | 0 |  |
| `tempnam` | call | 0 |  |
| `tmpfile` | call | ret |  |
| `ftruncate` | call | 0 |  |
| `vfprintf` | call | 0 |  |
| `fprintf` | call | 0 |  |

## file-delete — file-delete
| function | kind | arg | note |
|---|---|---|---|
| `unlink` | call | 0 |  |
| `rmdir` | call | 0 |  |

## file-perms — file-perms
*privilege/ownership changes on a path*

| function | kind | arg | note |
|---|---|---|---|
| `chmod` | call | 0 |  |
| `chown` | call | 0 |  |
| `chgrp` | call | 0 |  |
| `lchown` | call | 0 |  |
| `lchgrp` | call | 0 |  |
| `umask` | call | 0 |  |

## directory — dir-listing-traversal
| function | kind | arg | note |
|---|---|---|---|
| `scandir` | call | 0 |  |
| `opendir` | call | 0 |  |
| `readdir` | call | 0 |  |
| `glob` | call | 0 | glob:// wrapper |
| `dir` | call | 0 |  |
| `chdir` | call | 0 |  |
| `chroot` | call | 0 |  |

## ssrf-network — SSRF
*outbound request to an attacker-chosen host; block link-local/metadata/private*

| function | kind | arg | note |
|---|---|---|---|
| `fsockopen` | call | 0 |  |
| `pfsockopen` | call | 0 |  |
| `stream_socket_client` | call | 0 |  |
| `stream_socket_server` | call | 0 |  |
| `curl_exec` | call | recv | URL set via curl_setopt CURLOPT_URL |
| `curl_multi_exec` | call | recv |  |
| `curl_setopt` | call | 2 | CURLOPT_URL |
| `get_headers` | call | 0 |  |
| `http_get` | call | 0 |  |
| `http_post_data` | call | 0 |  |
| `gethostbyname` | call | 0 |  |
| `gethostbynamel` | call | 0 |  |
| `dns_get_record` | call | 0 |  |
| `checkdnsrr` | call | 0 |  |
| `getmxrr` | call | 0 |  |
| `SoapClient` | method | 0 | WSDL URL => SSRF/XXE |
| `stream_context_create` | call | 0 | can set follow_location/proxy |

## sql — SQLi
*raw SQL — use parameterized queries*

| function | kind | arg | note |
|---|---|---|---|
| `mysqli_query` | call | 1 |  |
| `mysqli_multi_query` | call | 1 |  |
| `mysqli_real_query` | call | 1 |  |
| `mysqli_prepare` | call | 1 | unsafe if concatenated |
| `mysql_query` | call | 0 |  |
| `mysql_unbuffered_query` | call | 0 |  |
| `mysql_db_query` | call | 1 |  |
| `pg_query` | call | 1 |  |
| `pg_send_query` | call | 1 |  |
| `sqlite_query` | call | 1 |  |
| `sqlite_unbuffered_query` | call | 1 |  |
| `sqlsrv_query` | call | 1 |  |
| `oci_parse` | call | 1 |  |
| `odbc_exec` | call | 1 |  |
| `odbc_prepare` | call | 1 |  |
| `db2_exec` | call | 1 |  |
| `ibase_query` | call | 1 |  |
| `mssql_query` | call | 0 |  |
| `maxdb_query` | call | 1 |  |
| `ingres_query` | call | 1 |  |
| `query` | method | 0 | PDO::query / mysqli::query |
| `exec` | method | 0 | PDO::exec (method form) |

## ldap — LDAP-injection
| function | kind | arg | note |
|---|---|---|---|
| `ldap_search` | call | 2 |  |
| `ldap_list` | call | 2 |  |
| `ldap_read` | call | 2 |  |
| `ldap_bind` | call | 1 | auth bypass via crafted DN/empty pw |
| `ldap_add` | call | 2 |  |
| `ldap_modify` | call | 2 |  |
| `ldap_delete` | call | 1 |  |

## xxe-xml — XXE
*external entity expansion => file read/SSRF; disable entities*

| function | kind | arg | note |
|---|---|---|---|
| `simplexml_load_string` | call | 0 |  |
| `simplexml_load_file` | call | 0 |  |
| `xml_parse` | call | 1 |  |
| `xml_parse_into_struct` | call | 1 |  |
| `loadXML` | method | 0 | DOMDocument::loadXML |
| `load` | method | 0 | DOMDocument::load path/URL |
| `loadHTMLFile` | method | 0 |  |
| `open` | method | 0 | XMLReader::open |
| `libxml_set_external_entity_loader` | call | cb:0 |  |
| `transformToXml` | method | 0 | XSLTProcessor + registerPHPFunctions => RCE |
| `loadHTML` | method | 0 | DOMDocument::loadHTML — HTML parse; entity/xxe surface |
| `setEntityLoader` | method | cb:0 | libxml external entity loader |
| `setParserProperty` | method | 0 | XMLReader — can enable DTD/entity loading |

## xss-output — XSS
*raw output of unescaped data; escape per context*

| function | kind | arg | note |
|---|---|---|---|
| `echo` | construct | self |  |
| `print` | construct | self |  |

## xss-output-call — XSS-or-info
| function | kind | arg | note |
|---|---|---|---|
| `printf` | call | 0 | user format-string bug |
| `vprintf` | call | 0 |  |
| `print_r` | call | 0 | outputs unless 2nd arg true |
| `var_dump` | call | * |  |
| `var_export` | call | 0 | outputs unless 2nd arg true |
| `exit` | call | 0 |  |
| `die` | call | 0 |  |
| `htmlspecialchars_decode` | call | 0 | UNDOES escaping |
| `html_entity_decode` | call | 0 | UNDOES escaping |

## header-redirect — header-injection-open-redirect
| function | kind | arg | note |
|---|---|---|---|
| `header` | call | 0 | Location: => open redirect; CRLF pre-5.1.2 |
| `setcookie` | call | 1 |  |
| `setrawcookie` | call | 1 |  |
| `session_set_cookie_params` | call | 0 |  |

## variable-injection — variable-injection
*imports attacker keys into scope (register_globals-style)*

| function | kind | arg | note |
|---|---|---|---|
| `extract` | call | 0 |  |
| `parse_str` | call | 0 | 1-arg form pollutes caller scope |
| `mb_parse_str` | call | 0 |  |
| `import_request_variables` | call | 0 |  |
| `compact` | call | * | info leak if keys tainted |

## regex-redos — ReDoS-or-injection
*catastrophic backtracking with a user pattern; deprecated ereg*

| function | kind | arg | note |
|---|---|---|---|
| `preg_match` | call | 0 | user PATTERN => ReDoS |
| `preg_match_all` | call | 0 |  |
| `preg_split` | call | 0 |  |
| `preg_grep` | call | 0 |  |
| `ereg` | call | 0 |  |
| `eregi` | call | 0 |  |
| `ereg_replace` | call | 0 |  |
| `eregi_replace` | call | 0 |  |
| `mb_ereg` | call | 0 |  |

## mail-injection — mail-header-injection
| function | kind | arg | note |
|---|---|---|---|
| `mail` | call | 3 | additional_headers => header injection |
| `mb_send_mail` | call | 3 |  |
| `imap_mail` | call | 3 |  |
| `ezmlm_hash` | call | 0 |  |

## weak-crypto — weak-crypto
*fast/broken for passwords/tokens/integrity*

| function | kind | arg | note |
|---|---|---|---|
| `md5` | call | 0 |  |
| `sha1` | call | 0 |  |
| `crc32` | call | 0 |  |
| `crypt` | call | 0 | weak without proper salt/algo |
| `hash` | call | 1 | md5/sha1 for secrets |
| `mcrypt_encrypt` | call | 0 | deprecated, ECB default |
| `mcrypt_decrypt` | call | 0 |  |

## weak-random — weak-random
*predictable; unfit for tokens/nonces/passwords*

| function | kind | arg | note |
|---|---|---|---|
| `rand` | call | ret |  |
| `mt_rand` | call | ret |  |
| `mt_srand` | call | 0 |  |
| `srand` | call | 0 |  |
| `uniqid` | call | ret |  |
| `lcg_value` | call | ret |  |
| `str_shuffle` | call | 0 |  |
| `array_rand` | call | 0 |  |
| `shuffle` | call | 0 |  |

## info-disclosure — info-disclosure
| function | kind | arg | note |
|---|---|---|---|
| `phpinfo` | call | * |  |
| `debug_zval_refcount` | call | 0 |  |
| `debug_backtrace` | call | ret |  |
| `debug_print_backtrace` | call | * |  |
| `getenv` | call | 0 |  |
| `get_cfg_var` | call | 0 |  |
| `ini_get_all` | call | ret |  |
| `get_defined_vars` | call | ret |  |
| `getmypid` | call | ret |  |
| `posix_getpwuid` | call | 0 |  |
| `posix_getgrgid` | call | 0 |  |
| `posix_getpwnam` | call | 0 |  |

## runtime-config — config-tamper
*changes PHP/security settings at runtime*

| function | kind | arg | note |
|---|---|---|---|
| `ini_set` | call | 0 |  |
| `ini_alter` | call | 0 |  |
| `ini_restore` | call | 0 |  |
| `putenv` | call | 0 |  |
| `set_include_path` | call | 0 |  |
| `apache_setenv` | call | 0 |  |
| `error_reporting` | call | 0 |  |
| `assert_options` | call | 0 |  |
| `setlocale` | call | 1 |  |

## process-control — process-control
*signal/priv/process manipulation*

| function | kind | arg | note |
|---|---|---|---|
| `posix_kill` | call | 0 |  |
| `posix_setuid` | call | 0 |  |
| `posix_setgid` | call | 0 |  |
| `posix_seteuid` | call | 0 |  |
| `pcntl_fork` | call | * |  |
| `pcntl_signal` | call | cb:1 |  |
| `proc_nice` | call | 0 |  |
| `proc_terminate` | call | 0 |  |
| `apache_child_terminate` | call | * |  |

## reflection-invoke — dynamic-invoke
*instantiate/invoke arbitrary class/method (gadget reach)*

| function | kind | arg | note |
|---|---|---|---|
| `newInstance` | method | * | ReflectionClass::newInstance |
| `newInstanceArgs` | method | * |  |
| `invoke` | method | * | Reflection*::invoke |
| `invokeArgs` | method | * |  |
| `setAccessible` | method | 0 | bypasses visibility |

## type-juggling — auth-bypass-type-juggle
*loose comparison / magic-hash / non-strict search => auth bypass*

| function | kind | arg | note |
|---|---|---|---|
| `in_array` | call | 2 | non-strict (3rd arg false) => 0==string |
| `array_search` | call | 2 | non-strict |
| `strcmp` | call | 0 | array arg returns NULL, NULL==0 true |
| `strcasecmp` | call | 0 |  |
| `strncmp` | call | 0 |  |
| `hash_equals` | call | 0 | SAFE — the correct constant-time compare |

## archive-extraction — zip-slip-path-traversal
*extracting an archive writes entry paths; a `../` entry escapes the target dir (Zip Slip). Also symlink entries.*

| function | kind | arg | note |
|---|---|---|---|
| `extractTo` | method | 0 | ZipArchive/PharData::extractTo — writes each entry; ../ escapes dir |
| `addFile` | method | 0 | path added to archive |
| `addFromString` | method | 0 | entry name is attacker-influenced |
| `buildFromDirectory` | method | 0 | PharData |
| `convertToExecutable` | method | * | PharData -> executable phar |

## image-processing — image-rce-ssrf
*Imagick reads by URL (SSRF) and historically had delegate RCE (ImageTragick, MVG/MSL); GD from-URL is SSRF.*

| function | kind | arg | note |
|---|---|---|---|
| `readImage` | method | 0 | Imagick::readImage — URL => SSRF; format abuse |
| `readImageBlob` | method | 0 |  |
| `readImageFile` | method | 0 |  |
| `setImageFormat` | method | 0 |  |

