# POP-gadget (deserialization) chain hunt

A dangerous `unserialize` is only exploitable if a **gadget chain** exists: a
class whose **magic method** — auto-invoked by unserialize (`__wakeup`,
`__unserialize`), by object destruction (`__destruct`), or by later use
(`__toString`, `__call`) — reaches a sink. This hunt enumerates every magic method
and traces it, interprocedurally, to any dangerous call.

## Method (XERJ data first)

`gadget_hunt.py` scrolls the `wpaudit` call graph, selects the 15 magic-method
names, and DFS-walks each (unambiguous call edges) to a **payoff**: a reached
function whose `sinks` are non-empty or whose `calls` include a high-impact
function (`exec`/`eval`/`unserialize`/`file_put_contents`/`unlink`/
`call_user_func`/`extractTo`/…). Flags the *auto-triggered* magic methods
separately — those are the live deserialization gadgets.

## Result on WordPress core

| | |
|---|--:|
| magic methods in core | 131 |
| **auto-triggered (`__wakeup`/`__unserialize`/`__destruct`/`__toString`) reaching a sink** | **0** |
| any magic method reaching a dangerous call | 2 |

The 2 are `__callStatic` (`pluggable-deprecated.php`, `AbstractEnum.php`), and both
are **false positives** on reading:
```php
public static function __callStatic($name, $arguments) {
    _deprecated_function(__CLASS__.'::'.$name, '3.5.0', '…');   // just a deprecation notice
}
```
The `call_user_func_array` at the end of the traced path
(`__callStatic → _deprecated_function → wp_trigger_error → do_action → _wp_call_all_hook → call_user_func_array`)
is WordPress's **generic hook dispatcher** firing registered callbacks — not
attacker-controlled from the magic method. And `__callStatic` is invoked by an
undefined *static* call, **not** by `unserialize`.

**Impact — a clean positive:** WordPress **core has no deserialization POP-gadget
chain.** Even given arbitrary object injection, no core magic method escalates it
to RCE/file-write/SSRF. The `__wakeup` methods that exist (e.g. SimplePie's
`FilteredIterator`) are inert or *defensive* (they throw to block deserialization).
Live gadget risk in the WP ecosystem comes from **vendored libraries and plugins**,
not core magic methods — point the same hunt at those.

## XERJ vs grep+context (measured)

The question — *"which magic methods reach a dangerous sink?"* — is inherently
**interprocedural**, so grep alone can't answer it; you must read the bodies and
hand-trace callees across files.

| approach | what it costs | tokens |
|---|---|--:|
| grep + read | read all **83 files** that define a magic method **and trace callees across files** | **~562,000** (> a context window → chunk → lose cross-file traces) |
| **XERJ** | query the 15 magic-method names + traverse the pre-built call graph → the answer | **~144** (2 candidates) |

**≈3,900× to the answer.** Quality is *higher*, not just cheaper: XERJ followed
6-hop cross-file chains reliably; a human/grep pass over 562k tokens would chunk
the code and is exactly where an interprocedural gadget chain slips through. The
call graph is built once and reused for every such query (gadgets, taint,
reachability).

## Reproduce
```bash
python3 gadget_hunt.py     # magic methods -> dangerous call, over the XERJ graph
```
Generalize by extending `MAGIC`/`DANGER`; point `wpaudit` at a plugin/vendored lib
to find the gadgets that core doesn't have.


## Correction (per-method scan supersedes the first pass)

The result above used `byname` (first definition per name) — a **name-collision
bug**: only 1 of 12 `__destruct` methods was analyzed, so it under-reported.
`magic_unsafe.py` scans **each** magic method's own body against the full 287-fn
catalog and is the authority. Corrected finding:

- **44 magic methods contain a dangerous call inside.** The one that matters is
  **`WP_HTML_Token::__destruct` → `call_user_func($this->on_destroy, $this->bookmark_name)`**
  (both properties settable) — a **real POP-gadget shape** (unserialize a token
  with `on_destroy='system'`, `bookmark_name='id'` → RCE on destruct).
- **It is defused — empirically verified.** The class adds `__wakeup(){ throw … }`
  (WP 6.4.2). Tested in real PHP 7.4/8.0/8.3 (`GADGET-WAKEUP-TEST.md`): a throwing
  `__wakeup` **suppresses `__destruct`** (PHP skips the destructor of an object whose
  `__wakeup` threw), direct AND nested — so the gadget genuinely cannot fire. The
  control (same `__destruct`, no `__wakeup`) DID fire, proving the guard is what
  stops it. (This corrects an earlier note that speculated the `__destruct` survives
  the throw — it does not, in modern PHP.)
- Everything else is benign/not-a-gadget: IRI/Iri `__set`/`__unset` `call_user_func`
  is internal `[$this,'set_'.$name]` dispatch (bounded, and `__set` isn't
  unserialize-triggered); `imagick`/`PHPMailer` `__destruct` do cleanup
  (`->clear()`, `smtpClose()`), no sink; `get_headers`/`debug_backtrace` are
  method-name collisions.

**All dangerous calls found INSIDE magic methods, by class:**

| class | calls (count) | assessment |
|---|---|---|
| RCE-code-callable | `call_user_func` ×5, `set_error_handler` ×1, `array_filter` ×1 | 1 real gadget shape (HTML_Token, **defused**); rest internal dispatch |
| auth-bypass-type-juggle | `in_array` ×25 | non-strict `in_array` in `__get/__isset/__set/__unset` — property-name allow-list checks, low severity |
| weak-crypto | `md5` ×11 | SimplePie `__toString` object-identity hashing — benign (not passwords) |
| info-disclosure | `debug_backtrace` ×1 | SimplePie `__call` debug — FP |
| SSRF | `get_headers` ×1 | method-name collision — FP |

**Net:** core has **one** genuine deserialization gadget shape and it is
explicitly guarded; no usable POP chain via core magic methods.
