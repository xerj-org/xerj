# The complete PHP security guide (for the xerj-security-audit skill)

A whitebox PHP audit has **two axes**, and both must be covered to claim
completeness:

1. **Dangerous functions (sinks)** — 287 built-ins/constructs, one call = one
   candidate. Enumerable and *provable* by AST census. See
   [`PHP-DANGEROUS-FUNCTIONS.md`](PHP-DANGEROUS-FUNCTIONS.md) /
   `php_dangerous_functions.json`.
2. **Dangerous patterns** — vulnerabilities that are **not a function call**:
   language semantics (`==` juggling), input *shape* (`?login[]=`), and semantic
   attacks (SQL truncation). These the sink census misses entirely. Catalogued in
   `php_dangerous_patterns.json`; the AST-detectable ones are censused by
   `pattern_census.py` into the `wppatterns` index.

> **Rule of thumb:** a sink tells you *where danger can happen*; a pattern tells
> you *how safe-looking code is actually exploitable*. You need both.

Measured on WordPress core: **11,223 sink sites** (proven 0-gap) + **2,077
pattern hits** across 10 AST-detectable patterns, + the manual/semantic classes
below that require reasoning.

---

## Part 1 — Dangerous functions (the sink axis)

Full table in [`PHP-DANGEROUS-FUNCTIONS.md`](PHP-DANGEROUS-FUNCTIONS.md). 287
functions across 30 categories, each with vuln class, safe/unsafe recipe, and the
taint-relevant argument: command/code exec, dynamic callables (`array_map`,
`preg_replace_callback`), `unserialize`, include loaders, file read/write/delete/
perms, directory, SSRF, SQL drivers, LDAP, XXE, variable-injection, ReDoS/ereg,
mail, header/redirect, weak crypto/random, info-disclosure, runtime-config,
process-control, reflection-invoke, type-juggle functions, output/XSS. Coverage is
*proven*: every call site is enumerated and reconciled against grep to 0 gaps.

---

## Part 2 — Dangerous patterns (the class the sink axis misses)

### A. Type juggling & loose comparison
- **Loose `==` / `!=`** (`loose-eq`) — `'0e123' == '0e456'` (both are `0` as
  floats → **magic hash** auth bypass); `'0'==false`, `null==0`, `'1abc'==1`,
  `0=='abc'` (pre-PHP8). **Detect:** AST `binary_expression` with `==`/`!=`/`<>`;
  flag when a side is a `hash()`/`md5()`/token/password. **Safe:** `===`,
  `hash_equals()`, `password_verify()`. *(WP core: 1,373 loose-`==`.)*
- **`switch()` uses loose `==`** (`switch-loose`) — `switch($_GET['role'])`
  matches `case 0` for `'0abc'`/`true`. **Safe:** `match(true)` / cast+validate.
- **`strcmp`/`strcasecmp`/`strncmp` with an array arg returns `NULL`**
  (`strcmp-array`) — `?pw[]=x` makes `$_GET['pw']` an array; `strcmp(array,$x)`
  → `NULL`; `if(strcmp(...)==0)` → `NULL==0` → **TRUE → bypass**. **Safe:**
  `is_string()` guard; `hash_equals`; `===0`.
- **`in_array`/`array_search` without `strict=true`** (`in-array-loose`) —
  `in_array('1abc',[1,2,3])` is TRUE; allow-list bypass. **Safe:** pass `true`.
  *(WP core: 124.)*
- **Non-constant-time secret compare** (`timing-compare`) — `$mac == $expected`
  leaks timing *and* juggles. **Safe:** `hash_equals`. *(WP core: 56.)*

### A2. Escaper / defense that is NOT a defense
- **`escapeshellarg` does not stop argument/option injection** (`option-injection`)
  — it quotes a value so it can't start a *new command*, but the value can still
  be read as an **option**. `exec('tar '.escapeshellarg($f))` with
  `$f='--checkpoint-action=exec=sh'` runs a command; `curl … escapeshellarg($url)`
  with `$url='-o/var/www/x.php'` writes a file (RCE). **Found in WP** `class-snoopy.php`
  — `$URI` escaped but placed after curl flags with no `--`. **Safe:** literal `--`
  before user args; reject leading `-`; argv-form `proc_open`.
- **`escapeshellcmd` is weaker than `escapeshellarg`** (`escapeshellcmd-weak`) —
  escapes command metachars but leaves argument splitting/quoting; misused as if
  it quoted an argument.

### A3. Class-based sinks (danger inside methods, not free functions)
- **Zip Slip** (`zip-slip`) — `ZipArchive/PharData::extractTo($dir)` writes each
  entry by its own path; a `../` entry escapes `$dir` → arbitrary write (RCE). WP
  core avoids this by hand-rolling extraction with path checks in `unzip_file`.
- **ImageMagick / Imagick** (`image-rce-ssrf`) — `Imagick::readImage`/`readImageBlob`
  on attacker-uploaded content → ImageTragick (MVG/MSL/SVG delegate) SSRF/RCE if
  ImageMagick is unpatched or `policy.xml` missing. **Found in WP** `class-wp-image-editor-imagick.php`.
- **phar:// via any file op** (`phar-wrapper-op`) — a benign `file_exists`/
  `getimagesize` on `phar://x.jpg` triggers `unserialize` of Phar metadata.
- The census catalogues these as method/ctor sinks (`extractTo`, `loadHTML`,
  `readImage`, `new ZipArchive`, `new Imagick`, …) so they're enumerated too.

### B. Input shape (the `?param[]=` family)
- **Array injection** (`array-injection`) — sending `login[]=a` makes
  `$_POST['login']` an **array**. It breaks every string-typed sink:
  `preg_match` returns `false`, `strcmp`/`md5`/`hash` return `NULL`+warning, SQL
  interpolates the literal `Array`, and `is_string()`-less validators are
  bypassed. The single most under-tested PHP input bug. **Detect:** request params
  reaching string functions/SQL/regex with no `is_string()`/`is_scalar()`/`(string)`
  guard. **Safe:** validate/cast every request param to the expected type; reject
  arrays where scalars are expected.
- **Mass assignment / register_globals emulation** (`mass-assignment`) —
  `extract($_REQUEST)`, `foreach($_POST as $k=>$v){$$k=$v;}`,
  `$obj->fill($_POST)` overwrite variables/columns (`is_admin`). **Safe:** explicit
  field allow-list.
- **Variable variables** (`var-variable`) — `$$key` with request-controlled
  `$key` → arbitrary variable overwrite. **Safe:** allow-listed array lookup.

### C. SQL semantics (beyond the driver call)
- **SQL truncation** (`sql-truncation`) — register `admin` + 55 spaces + `x`;
  MySQL in non-STRICT mode silently truncates to the `VARCHAR(N)` column length →
  stored as `admin` → **duplicate/overwrite the admin account**. **Detect:**
  needs the column length — manual/schema. **Safe:** validate max length;
  `STRICT_ALL_TABLES`.
- **Multibyte charset SQLi** (`charset-sqli`) — GBK/Big5 + `addslashes` →
  `0xbf27` becomes a valid char + a free quote, escaping the escaper. **Safe:**
  parameterized queries; `set_charset('utf8mb4')`.
- **ORDER BY / LIMIT / identifier injection** (`order-by-inj`) — placeholders
  can't bind identifiers; `ORDER BY $_GET['sort']` = injection. **Safe:**
  allow-list columns. *(WP core: 22 candidate lines.)*
- **Second-order injection** (`second-order`) — safe on input, unescaped when
  later read from the DB and concatenated. **Safe:** escape at every sink; treat
  DB reads as untrusted.

### D. Type confusion & validation bypass
- **`is_numeric`/`intval`/`filter_var` bypass** (`is-numeric-bypass`) —
  `is_numeric('0x1A'|'1e3'|' 12')` can be TRUE; `(int)'123abc'==123`;
  `FILTER_VALIDATE_URL` accepts `javascript:`. **Safe:** `ctype_digit`; explicit
  FILTER flags; cast+re-render+compare.
- **`empty()`/`isset()` logic pitfalls** (`loose-empty-isset`) — `empty('0')` is
  TRUE → a legitimate `'0'` treated as missing in an auth branch. **Safe:**
  `=== ''`, `array_key_exists`.

### E. Regex footguns
- **Anchor / multiline / dot bypass** (`regex-anchor-bypass`) — `/^ok$/`: `$`
  matches before a trailing `\n`; `.` doesn't match `\n`, so a newline payload
  bypasses. **Safe:** `\A...\z`, `/D`, reject newlines.
- **PCRE backtrack returns `false`, not `0`** (`pcre-backtrack`) — on catastrophic
  backtracking `preg_match` returns `false`; `if(preg_match($bad,$x))` treats it as
  no-match → validator bypass + ReDoS. **Safe:** check `===1`; bound input length.
- **`preg_replace /e`** — eval'd replacement (see the sink catalog).

### F. Files, paths, races
- **Null-byte truncation** (`null-byte`) — `secret.php%00.jpg` truncates in file
  ops (pre-5.3.4); `%00` splits validators. **Safe:** reject `\0`; PHP ≥ 5.3.4.
- **Path traversal encodings** (`path-traversal`) — `../`, `..%2f`, `%2e%2e`,
  `....//`, absolute override. **Safe:** `realpath()` under a verified base;
  `basename`+allow-list.
- **phar:// deserialization** (`phar-deser`) — `phar://x.jpg` triggers
  `unserialize` of Phar metadata via **any** file op → POP chain. **Safe:** deny
  wrappers; `Phar::interceptFileFuncs`.
- **TOCTOU race** (`toctou`) — `is_writable($f)` then `fopen($f)`; attacker swaps
  via symlink. **Safe:** operate on fds; atomic ops.
- **Upload → exec** (`upload-exec`) — `shell.php`/`.phtml`/double-ext/MIME-spoof/
  `.htaccess` in a served dir. **Safe:** random name; ext+content allow-list;
  non-exec storage.

### G. Auth, session, trust boundaries
- **CSRF absence** (`csrf-absence`) — state change reachable with no verified
  nonce/token. **Detect:** absence property (the authz graph). **Safe:** per-user
  per-action nonce.
- **Session fixation** (`session-fixation`) — no `session_regenerate_id(true)`
  after login. **Safe:** regenerate on every privilege change.
- **Missing cookie flags** (`cookie-flags`) — `setcookie` without HttpOnly/Secure/
  SameSite. **Safe:** set all three. *(WP core: 44 candidate calls.)*
- **Host/X-Forwarded trust** (`host-header`) — `$_SERVER['HTTP_HOST']` /
  `X-Forwarded-*` used in URLs, reset links, or access decisions → poisoning.
  **Safe:** fixed site URL; trust only vetted proxies. *(WP core: 25.)*
- **Open redirect** (`open-redirect`) — `Location: $_GET['next']` to `//evil`,
  `\evil`, `javascript:`. **Safe:** allow-list host; `wp_safe_redirect`.

### H. Output / XSS context
- **Wrong-context escaping** (`xss-context`) — `htmlspecialchars` without
  `ENT_QUOTES` in an attribute; `esc_html` on a JS/URL/CSS value; JSON in
  `<script>` without `JSON_HEX_TAG`. **Safe:** context-correct escaper.

### I. Footguns
- **`@` error suppression** (`error-suppression`) — `@unserialize`/`@$_GET` hides
  security-relevant failures. **Safe:** handle errors. *(WP core: 419.)*
- **User enumeration** (`user-enum`) — distinct message/timing for valid vs
  invalid users. **Safe:** uniform responses.

---

## Part 3 — Coverage: what's provable vs what needs reasoning

| detection kind | classes | how the skill covers it |
|---|---|---|
| **AST (provable census)** | all 275 sinks; loose-`==`, `in_array`, `strcmp`-array, `@`, var-vars, cookie-flags, `switch`-on-request | `sink_census.py` (0-gap proof) + `pattern_census.py` → `wpsinks`/`wppatterns` |
| **regex/query** | ORDER-BY injection, Host-header trust, open-redirect | pattern census + XERJ queries over facts |
| **manual / semantic** | SQL truncation, charset SQLi, second-order, phar, TOCTOU, upload-exec, is_numeric bypass, regex-anchor, session-fixation, xss-context, user-enum | agent reasoning on the flagged code + schema/runtime; verify by reading then executing |

**The honest coverage statement:** *sink calls are enumerated with a proven
zero-gap census; the AST-detectable patterns are censused into `wppatterns`; the
semantic/manual pattern classes are catalogued with detection guidance and swept
by agent reasoning — no dangerous class is un-catalogued.* That last clause is
what "we know ALL the functions and patterns" means, honestly.

## Reuse
```bash
python3 sink_census.py    <src>   # 275 functions -> wpsinks (0-gap proof)
python3 pattern_census.py <src>   # AST-detectable patterns -> wppatterns
python3 enrich_pipeline.py        # AI verdicts on the high-risk of both
python3 trace_sink.py <file> <line>
```
Extend `php_dangerous_functions.json` and `php_dangerous_patterns.json` for your
framework/language — the model is data, not code.
