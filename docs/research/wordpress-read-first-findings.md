# Reading-first core audit: what the detectors couldn't see

The structural detectors ([authz graph](wordpress-authz-agentic-audit.md),
check-vs-use, double-prepare) concluded core is clean — because they only match
*shapes*. Logic bugs live in *semantics*: a defense that is present but
**incomplete**. This pass switched mode — the agent used XERJ only to *navigate*
to logic-heavy security functions, then **read them in full and reasoned about
exploitability**, the way a human auditor does. Then each finding was turned back
into a XERJ detector.

## Confirmed solid on close reading (negative results, stated honestly)

- **`wp_validate_redirect`** — the allowlist regex strips backslashes (kills the
  `\`→`/` browser open-redirect bypass), `_deep_replace` strips CRLF recursively
  (kills `%0%0a0a`), and `@`-userinfo tricks resolve to the real host, which is
  allow-list-checked. Robust.
- **`maybe_unserialize` / `maybe_serialize`** — symmetric for objects (both use
  `/^O:[0-9]+:/`), so a string that *looks* serialized is double-serialized on
  write and returns as a string on read. String-meta PHP Object Injection is
  defended for data written through the pair.
- **Widget instance `unserialize`** (customizer) — gated by
  `hash_equals( wp_hash($decoded), $instance_hash_key )` before `unserialize`.
  An attacker can't forge the secret-key HMAC, so POI is defended.

## The finding: SSRF to cloud metadata via `wp_http_validate_url`

`wp_http_validate_url()` is core's SSRF gatekeeper for `wp_safe_remote_*` and
pingbacks. Its private-IP blocklist:

```php
if ( 127 === $parts[0] || 10 === $parts[0] || 0 === $parts[0]
    || ( 172 === $parts[0] && 16 <= $parts[1] && 31 >= $parts[1] )
    || ( 192 === $parts[0] && 168 === $parts[1] )
) { /* reject unless http_request_host_is_external allows */ }
```

Reading the *coverage* (not the shape) shows the gap: it blocks loopback, 10/8,
0/8, 172.16/12, 192.168/16 — but **not `169.254.0.0/16` (link-local)**, which
contains **`169.254.169.254`, the AWS / GCP / Azure cloud-metadata endpoint**
(and `100.100.100.200`, Alibaba). Verified against the exact logic:

| target | verdict |
|---|---|
| 127.0.0.1, 10.x, 172.16.x, 192.168.x | BLOCKED |
| **169.254.169.254 (cloud metadata)** | **ALLOWED** |
| 100.64/10 (CGNAT) | ALLOWED |

**Impact.** Any core feature or plugin that passes a user-supplied URL through
`wp_safe_remote_get()` / the pingback path can be steered at the metadata service
→ theft of cloud IAM credentials. Also missing: CGNAT `100.64/10` and IPv6
(loopback `::1`, ULA `fc00::/7`) — though IPv6 host *literals* are separately
rejected by the `strpbrk(host, ':#?[]')` check.

**Honest status.** This is a real, verifiable gap, but a *known-class* one: core
historically treats `wp_http_validate_url` as best-effort and points hardening at
the `http_request_host_is_external` filter, and exploitation needs the SSRF
feature reachable in a cloud environment. It is not claimed as a novel 0-day. It
is exactly the kind of semantic-completeness weakness that **only a read** finds —
and that is the point.

## Improving XERJ from the finding

The bug class is *an incomplete allow/deny list in a security validator*.
`docs/examples/ast-vuln-graph/wp_ssrf_ranges.py` encodes it: locate IP-range SSRF
validators (host resolution + octet comparisons), then check their coverage
against the set of ranges every validator **should** block, and report what's
**missing**. Run against core it independently reproduces the finding:

```
### wp_http_validate_url  (wp-includes/http.php)
    [x] 127.0.0.0/8   [x] 10/8   [x] 172.16/12   [x] 192.168/16
    [ ] 169.254.0.0/16 link-local/METADATA   <<< MISSING
    [ ] 100.64.0.0/10 CGNAT                   <<< MISSING
    [ ] IPv6 ::1 / ULA fc00::/7               <<< MISSING
    !! SSRF TO CLOUD METADATA POSSIBLE
```

This is the loop worth building for XERJ as an audit substrate: a human (or an
AI) reads and finds a semantic gap once; the finding is encoded as a
completeness check; XERJ then carries it across every future audit — of core
upgrades and, more usefully, of the plugin ecosystem, where SSRF validators are
frequently hand-rolled and far more incomplete than core's.

### The general lesson for XERJ

Structural detectors answer "is the defense *present*?" The bugs that survive
audit are "is the defense *complete*?" — a missing IP range, an unescaped context,
an un-re-verified object relationship. XERJ's contribution is to make each such
semantic invariant, once discovered by reading, a **stored, queryable
completeness check** over the whole indexed codebase. Precision comes from the
read; durability and scale come from XERJ.

---

# Part 2 — Sanitizer composition, and making the audit cheap

## The class: two protections that interact and one undoes the other

The richest composition bug is **de-escape after escape before a sink**: a value
is escaped, then a later filter (`stripslashes`, `wp_unslash`, `urldecode`,
`html_entity_decode`) *removes* the escaping before it reaches SQL or output.
`esc_sql($x)` then `stripslashes(...)` = injection; `esc_html($x)` then
`html_entity_decode(...)` = XSS.

Reading core's compositions, the invariant it holds everywhere is **escaper-last
before the sink**:

- `WP_Term_Query::get_terms` — `sanitize_term_field` (slashed) → `stripslashes`
  (clean) → **`esc_sql` last** → query. Correct; the comment documents it.
- `wp_update_term` — `sanitize_term` → `wp_unslash` → **`$wpdb->update()`**, which
  re-escapes via format specifiers. Unslashing *before* `$wpdb->update/insert` is
  the *required* WP convention (not doing it double-slashes). Correct.
- `wp_widget_rss_output` — `$desc = html_entity_decode(...)` then
  `$desc = esc_attr( wp_trim_words($desc) )` **re-escapes last**. Correct.

An order-aware detector flagged **23** candidates; every one cleared on reading,
via four false-positive drivers now understood: (1) a *safe* self-escaping
`$wpdb->update/insert/prepare` sink misread as raw; (2) decode-then-re-escape on
the actual output variable; (3) escape and decode on *different* variables; (4)
plaintext-email output where decode is correct. **Core composes correctly.** But,
as with SSRF, that verdict required *reading* — and reading 23 bodies is
expensive.

## Making it cheap: the sanitizer-sequence fingerprint (the XERJ improvement)

`wp_compose_index.py` computes, once at index time, each function's ordered
**sanitizer sequence** — a compact keyword list like
`["ESC:esc_html","DEE:html_entity_decode","ESC:esc_attr","SNKout"]` — plus a
`sink_raw` flag distinguishing attacker-dangerous sinks (`$wpdb->query`,
`get_results`, `echo`) from self-escaping ones (`$wpdb->update/insert/prepare`).
It stores these as indexed fields. The whole composition audit then becomes a
single structured query:

```
GET wpcompose  { compose_danger:true  AND  sink_raw:true }   →  6 candidates + their fingerprints
```

The fingerprints are **self-triaging**: `wp_update_term`'s
`[ESC:wp_slash, DEE:wp_unslash, …, SNKsafe]` shows the unslashed value reaching a
*safe* sink; `wp_notify_moderator`'s long `SNKout` tail is email. Most candidates
clear without pulling a single line of code.

**Measured token cost of the same audit:**

| approach | what it reads | tokens |
|---|---|--:|
| read every sanitizer-bearing function to find the bad order | 3,624 function bodies | **~1,329,000** |
| one query over the sanitizer-sequence fingerprint | 6 fingerprints (+ read only the un-clearable) | **~490** |

**≈2,700×.** The audit cost drops from O(total code size) to O(true candidates),
because the semantic invariant was compiled into an indexed fingerprint once and
queried, instead of re-derived by reading every time.

## The generalizable pattern for XERJ

This is the reusable shape for *every* invariant found by reading:

1. **Read once** to discover the invariant (escaper-last; block 169.254; re-verify
   the object relationship).
2. **Compile it to a fingerprint field** at index time — the ordered sanitizer
   sequence, the covered IP ranges, the checked-vs-used object keys.
3. **Audit by querying the fingerprint**, returning only violators — tiny result,
   no code transfer.
4. **Read only the survivors** to confirm.

Steps 2–4 are where XERJ turns a million-token reading pass into a few-hundred-token
query — and, run against the plugin ecosystem, the same fingerprints will surface
the escaper-last violations and incomplete validators that core doesn't have.
