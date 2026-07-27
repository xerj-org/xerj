# Per-file zero-day sweep (multi-agent workflow)

After the structural passes reported core "clean," a **per-file** review was run —
a multi-agent workflow over the 883 security-relevant files, each reviewer primed
to *assume bugs were missed and look harder*, with **adversarial verification** of
every candidate. This is the honest test of whether the structural approach had
blind spots. **It did — it found a real authz gap the graph missed.**

## The workflow
- **Scout** → the 883-file work-list (from the XERJ indices).
- **Review** → 177 agents (5 files each), reading real code + XERJ facts, hunting
  logic bugs / missing-insufficient-broken(`==`)-authz / second-order / TOCTOU /
  deser / type-confusion — reporting only concrete source→sink or broken-check
  candidates.
- **Verify** → an independent agent per candidate, tasked to **refute** it
  (reachable? guarded? dead code? receiver-type FP? deploy-only?).
- **Synthesize** → only survivors.

Scale: **222 agents, ~10.8M tokens, ~31 min**, 44 candidates → **5 confirmed**
after adversarial verification. (8 verify agents hit a usage limit near the end;
those candidates are unverified, not counted.)

## Confirmed findings

| # | file:line | class | severity | note |
|---|---|---|---|---|
| 1 | `wp-admin/user-new.php:100` | role injection (inconsistent `wp_ensure_editable_role`) | Medium (multisite + filtered roles) | **NEW — the structural graph missed it.** See FINDINGS.md #4 |
| 2 | `wp-includes/http.php:598` | SSRF `169.254` gap | Medium | independent re-discovery of FINDINGS.md #1 (validates the sweep) |
| 3 | `akismet/class.akismet-admin.php:1212` | CSRF (missing nonce on a GET view action) | Low | plugin, needs a logged-in admin + forged GET |
| 4 | `ID3/module.audio-video.flv.php:716` | resource-exhaustion DoS | Low | author+ uploads a crafted `.flv`; loop over attacker-set count |
| 5 | `build/pages/font-library/page.php:308` | missing capability check | Low | low-priv access to the font-library admin page |

Findings 3–5 are **at the workflow's adversarial-verifier confidence**; only #1
(and #2, already known) were re-read by the lead. #3–#5 are low-severity and
plugin/edge — reported honestly as candidates pending a lead read, not asserted.

## The honest takeaway — why the structural pass missed #1
My authz graph asked *"is there a capability check?"* — `user-new.php` has one
(`promote_user`), so it passed. The real bug is **inconsistency**: a second,
role-specific guard (`wp_ensure_editable_role`, added 6.8.0) is applied to 2 of 3
sibling role-sinks and **omitted on the third**. A "presence of a check" model
cannot see a *missing sibling guard*; a per-file read that compares the three
branches can. **Lesson: structural coverage and per-file reading are complementary
— the graph narrows and proves coverage; the read catches inconsistency and logic
gaps the facts don't encode.** This sweep is now part of the method.

## Cost
~10.8M tokens for the exhaustive per-file pass is the *thorough* end of the dial —
appropriate when you suspect misses. The cheap structural passes (~26k tokens for
the whole earlier audit) are the daily driver; this deep sweep is the periodic
"did we miss anything" backstop. Both belong in the workflow.

## Reproduce
The workflow script is under the session's `workflows/scripts/`; re-run points the
same reviewer/verifier prompts at any file list (`review_files.json`).

---

## Recovery after the usage-limit interruption

The run hit the weekly usage limit near the end. Reading the workflow
`journal.jsonl` (one `result` line per agent) recovers exactly what completed:

- **Review phase: 177/177 batches finished → all 883 files WERE analyzed.** No
  file was left unreviewed. (This is the important recovery result: the coverage
  of the *review* is complete.)
- **Verify phase: 36/44 candidates verified; 8 dropped** at the limit. So the gap
  is 8 *unverified candidates*, not unanalyzed files — recoverable by reading the
  code directly (no agents needed).

### The 8 dropped candidates — hand-verified by the lead (reading real code)

| candidate | lead verdict |
|---|---|
| `blocks/cover.php:102` (oEmbed KSES bypass) | **REFUTED** — the block builds a *fixed* `<iframe>` template with `esc_url($iframe_src)`; it does not emit the provider's raw HTML. Not injectable. |
| `feed-rss2-comments.php:107` (`]]>` CDATA breakout) | **Plausible, low–medium, unconfirmed.** `comment_text()` output sits inside `<![CDATA[…]]>`; `wp_kses_post` does not escape a literal `]]>`, so a comment containing `]]>` can close the CDATA and inject markup into the **comments RSS feed**. Trigger: unauthenticated comment on a post with comments-feed enabled; impact is on **feed consumers/aggregators** that render it unsanitized — not a WP-site compromise. Needs a feed-reader test to confirm; reported as a candidate. |
| `block-supports/custom-css.php:258` (edit_css scope) | **Plausible, medium, unconfirmed.** The custom-CSS strip is wired to `content_save_pre`/`content_filtered_save_pre` (post-content saves). If block-widget saves via the REST widgets path don't pass those filters, a user with `edit_theme_options` but not `edit_css`/`unfiltered_html` could store custom CSS. Trigger: authenticated `edit_theme_options` user, block-widget REST save. Needs the widget-save-path trace to confirm. |
| `rest .../widgets-controller.php:178` (context=edit scoping) | Candidate — read-scoping on `show_in_rest` sidebars; low, needs confirmation. |
| `rest .../global-styles-controller.php:256` (per-block css not validated) | Candidate — `validate_custom_css` only on top-level `styles.css`, low. |
| `rest .../abilities-v1-run-controller.php:163` (404-vs-403 info leak) | Candidate — ability-name enumeration; info-disclosure, low. |
| `wp-mail.php:170` (From-header → post author) | **Design-level**, by-documentation (Post-by-Email trusts the mailbox); not a code bug. |
| `wp-trackback.php:75` (UTF-7 charset filter) | WordPress **deliberately blocks** UTF-7 trackbacks here — the code is the mitigation, not a bug. Likely FP. |

**Net after recovery:** the one lead-confirmed *new* finding remains
`user-new.php` role injection (FINDINGS.md #4); `cover.php`, `wp-mail.php`,
`wp-trackback.php` are refuted/by-design; `feed-rss2-comments.php` and
`custom-css.php` are plausible-but-unconfirmed candidates worth a follow-up
(feed-reader test / widget-save-path trace) when the usage budget resets.

## How XERJ makes this discoverable vs. a full-context-window search

The naive alternative — feed the whole codebase to a model and ask "find bugs" —
fails on **three** axes this sweep exposes:

1. **It doesn't fit.** WP core is ~5.2M tokens; a 200k window holds ~4% of it.
   You *must* chunk, and chunking is where interprocedural bugs (the
   `user-new.php` role flows into `add_user_to_blog` two files away) get split
   apart and lost. XERJ holds the whole call graph as **facts** (~26k tokens for
   the structured audit) so the agent reasons over all 883 files without loading
   their text.
2. **It can't compare siblings without the map.** The `user-new.php` bug is a
   *missing sibling guard* — visible only when you see all three role-sinks
   together. XERJ's `wpsinks`/`wpauthz` indices return "every call to
   `add_option`/`add_existing_user_to_blog` with a role arg" in one query, so the
   reviewer is *handed the three siblings to compare*; a windowed scan reads them
   in different chunks and never lines them up.
3. **It re-reads everything every time.** A full-context pass pays the whole token
   cost per question. XERJ builds the index once and every subsequent query
   (sinks, patterns, authz posture, gadget reachability, taint) is ~hundreds to a
   few thousand tokens. The per-file *deep* sweep (~10.8M tokens, 222 agents) is
   the thorough backstop; the XERJ structured passes (~26k) are the daily driver —
   and XERJ is what let the sweep **target** the 883 security-relevant files
   instead of blindly chunking 1,492.

The division of labour: **XERJ narrows and proves coverage and hands the reviewer
the right neighbours to compare; the per-file read catches the logic/inconsistency
gaps the facts don't encode.** Neither alone found `user-new.php` — the structural
graph passed it (a cap check *was* present), and a blind full-context scan would
have split its cross-file flow. Together they caught it.
