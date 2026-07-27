# Empirical note: does a throwing `__wakeup` stop a `__destruct` gadget?

Ran the WP_HTML_Token gadget shape in **real PHP (Docker: 7.4.33, 8.0.30, 8.3.32)**
with a control. Script: [`sink-census/gadget_wakeup_test.php`](sink-census/gadget_wakeup_test.php).

```bash
docker run --rm -v "$PWD":/app php:8.3-cli php /app/gadget_wakeup_test.php
```

## Setup
- `Guarded` = WP_HTML_Token's shape: dangerous `__destruct`
  (`call_user_func($this->on_destroy, $this->bookmark_name)`) **+** a throwing
  `__wakeup`.
- `NoGuard` = the **control**: identical dangerous `__destruct`, **no** `__wakeup`.
- Payloads set `on_destroy='GADGET'` (a marker) and are unserialized directly and
  nested inside a plain `Container`.

## Result (identical on 7.4 / 8.0 / 8.3)
| case | class | outcome |
|---|---|---|
| A | NoGuard, direct | **GADGET FIRED** (`__destruct` ran `call_user_func`) |
| D | NoGuard, nested | **GADGET FIRED** |
| B | Guarded, direct | `__wakeup` throws → **gadget did NOT fire** |
| C | Guarded, nested | `__wakeup` throws → **gadget did NOT fire** |

## Conclusion (verified, not from memory)
- **A throwing `__wakeup` suppresses `__destruct`.** When `__wakeup` throws during
  `unserialize`, PHP treats the object as failed and **skips the destructor** —
  direct and nested, all tested versions. The "`__destruct` survives a `__wakeup`
  throw" folklore is **false for PHP ≥ 7.4** (may have held in PHP 5).
- **`WP_HTML_Token` is genuinely defused** by its `__wakeup` guard.
- The **control fired**, proving the gadget is real and the guard is what stops it.
  ⇒ a dangerous `__destruct` **without** a throwing `__wakeup` is a **live gadget**.

## Consequence for the gadget hunt
The exposed gadget inventory = classes with a dangerous magic method (esp.
`__destruct`/`__toString`/`__call`) that **lack** a throwing `__wakeup` guard (or
route through `__unserialize`, which this test did not cover). Build that list next.
