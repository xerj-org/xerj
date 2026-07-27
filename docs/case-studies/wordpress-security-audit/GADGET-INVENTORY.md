# Gadget inventory: unguarded dangerous magic methods + the autoloader question

Empirically established (`GADGET-WAKEUP-TEST.md`): a throwing `__wakeup`/`__unserialize`
suppresses `__destruct`. So the **exposed** deserialization gadgets are classes with
a dangerous *auto-fire* magic method (`__destruct`/`__toString`) that **lack** such a
guard. This inventories them — XERJ first, then manually, then tests the autoloader
bypass idea in real PHP.

## XERJ pass (`gadget_inventory.py`)
Groups every dangerous magic method by class and checks for a throwing
`__wakeup`/`__unserialize` in the same class.

| | count |
|---|--:|
| classes with a dangerous magic method | 43 |
| GUARDED (throwing `__wakeup`/`__unserialize`) | 4 |
| **EXPOSED (auto-fire magic, no guard)** | **12** |

The 12 exposed are **all `__toString`** (not `__destruct`): 11 SimplePie classes
doing `md5(...)` (object-identity hashing — benign) and one Requests `StreamTrait`
`set_error_handler` (internal callback). `__toString` only fires when the object is
**cast to string**, not automatically on unserialize. **No exposed `__destruct`.**

## Manual pass (grep + read) — cross-check
Read every `__destruct` in the whole tree (core + vendored):

```
__destruct classes: 12
DANGEROUS __destruct body: class-wp-html-token.php   __wakeup guard: throws
-> 1 dangerous __destruct, and it is GUARDED
```

**XERJ and manual agree exactly:** the only dangerous `__destruct` is
`WP_HTML_Token`, and it is guarded. There is **no unguarded dangerous `__destruct`
gadget** in core or the bundled libraries.

## The autoloader hypothesis — tested (`gadget_autoload_test.php`, PHP 8.3)
*"Could you autoload the class so it's destroyed without `__wakeup`?"* — No:
```
[autoloader] loading Guarded
caught: should never be unserialized      # __wakeup STILL fired -> __destruct suppressed
```
Whether a class is pre-loaded (bootstrap) or brought in on-demand by an autoloader,
`unserialize` runs its `__wakeup` once it's available. The autoloader only controls
**availability** — and core has no unguarded dangerous-`__destruct` class to make
available. So the guard is autoload-proof.

## Honest XERJ-vs-manual comparison (tokens & quality)
| | XERJ facts | manual grep+read |
|---|--:|--:|
| tokens | ~14,700 (all magic methods pulled) | **~649** (12 `__destruct` bodies) |
| result | 43 grouped, 4 guarded, 12 exposed | 1 dangerous `__destruct`, guarded |
| agreement | — | **identical on the `__destruct` question** |

**Honest call:** for this *narrow, low-cardinality* question (only 12 `__destruct`
methods, all short), **grep+read is cheaper** — XERJ's pull is broader than needed.
XERJ wins when the question is **broad, interprocedural, or reused** (every magic
method + call-graph reachability + taint over the same index), not for a single
small lookup. Use the cheaper tool for the question at hand; that's the honest rule.

## Verdict
Core has **zero unguarded dangerous `__destruct` gadgets**; the one dangerous
`__destruct` (`WP_HTML_Token`) is guarded by a throwing `__wakeup` that is
autoload-proof and (verified) suppresses `__destruct`. The `__toString` "exposed"
set is benign (`md5`) or internal. No usable core POP gadget exists — independent of
any delivery vector.
