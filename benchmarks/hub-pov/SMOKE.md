# Hub v2 proof of value: harness smoke run (2026-10-17)

> **These smoke numbers are NOT results.** They come from 1 repeat, 2 tasks (one of
> them a smoke fixture rather than a pre-registered task), a cheap model, a heavily
> loaded machine and a locally signed catalogue. They exist only to show that the
> harness runs and measures what [PLAN.md](./PLAN.md) says it measures. Do not quote
> them, compare arms with them, or publish them. The §7 gate was not evaluated
> (`pov.py report` refuses with fewer than 3 repeats).

## What ran

| | |
|---|---|
| Matrix | [`matrix/smoke.json`](./matrix/smoke.json): every arm A0–A7 × 2 tasks × 1 repeat = 16 cells, run serially |
| Tasks | `C1:snappy:T4` (registered suite, `kInputMarginBytes`, mechanical `check.py`); `C2:lz4.smoke:S1` (smoke-only fixture, LLM judge) |
| Agent | `claude -p` 2.1.302, `--model sonnet` (resolved: `claude-sonnet-5-5`), `--max-turns 15` (A0: 1) |
| xerj | `v1.0.0-rc.96` built from `main` at `b3753ddb` |
| Packs | the real releases `pack-lz4-0774d055`, `pack-snappy-9c28114a`, `pack-zstd-01b7154f`. Each archive's released `.sig` verified against the compiled-in `HUB_PUBKEY_HEX` before use. |
| Sandbox | every `claude -p` ran through [`sandbox.sh`](./sandbox.sh): bwrap, own PID and user namespace, private network namespace, `/home` masked |
| Run dirs | `/home/claude/hubdev-wt/pov-run/results/smoke-20261017b/` (the valid pass); `smoke-20261017/` is the first pass, kept because it exposed defects 8 and 9 |
| Spend | $0.40 for the agents across 16 cells; 241 s of agent wall time; about 5 min end to end |
| Host load (1-min) | **25 to 113** throughout. This is not a quiet machine, so every wall time below is invalid as a timing (PLAN §6). |

### The catalogue override (the live hub catalogue is not deployed)

`https://hub.xerj.org/api/v1/catalogue.json` returns the site's HTML page. The smoke run
therefore used the override that `tools/hub-packs/consumer_e2e.sh` uses:
`xerj hub build-catalogue` builds the catalogue and it is served over HTTP as
`<hub>/api/v1/catalogue.json` + `.sig`, with `XERJ_HUB_URL` pointing at it. Two
differences from the CI e2e, both forced by running locally:

1. **Throwaway signing key.** The hub seed is a CI secret, so the catalogue is signed
   with a throwaway ed25519 key, and the client is pointed at that key with
   `XERJ_HUB_PUBKEY`. This is the local-rehearsal override `consumer_e2e.sh` documents.
   The packs' signatures are re-made with the same key, because there is only one
   client key. The pack **bytes** are the released ones: `pack_url` points at the
   GitHub release, so the v2 arms download real bytes over the real network, and
   `pack_sha256` in the catalogue pins them. Their genuine release signatures were
   verified separately (see "Packs" above).
2. **The catalogue is served inside each jail** (`python3 -m http.server` on the jail's
   own `127.0.0.1:18080`), because the jail's network namespace cannot reach the host
   loopback (defect 1). The re-made `.sig` files are served from there too
   (`sig_url` is rewritten, then the catalogue is re-signed).

Both exports live only in the v2 `xerj` shim (`XERJ_HUB_URL`, `XERJ_HUB_PUBKEY`).
**Before the full run, switch to the deployed, really-signed catalogue** and drop both
from the shim.

## Smoke table (run `smoke-20261017b`; NOT results)

| arm | task | valid | grade | cost $ | out tok | turns | wall s* | bytes dl | xerj calls | routing (H2) | contamination check |
|---|---|---|---|---|---|---|---|---|---|---|---|
| A0 | C1 snappy T4 | yes | pass | 0.0137 | 406 | 1 | 6.3 | 15,824 | 0 | – | clean |
| A1 | C1 snappy T4 | yes | pass | 0.0168 | 264 | 2 | 7.2 | 1,183,155 | 0 | – | clean (fetched the pin with git) |
| A2 | C1 snappy T4 | yes | pass | 0.0173 | 268 | 2 | 6.8 | 151,260 | 0 | – | clean (raw.githubusercontent) |
| A3 | C1 snappy T4 | yes | pass | 0.0191 | 224 | 2 | 6.7 | 25,092 | 0 | – | clean (Sourcegraph only) |
| A4 | C1 snappy T4 | **NO** | pass | 0.0187 | 441 | 2 | 7.3 | 158,487 | 0 | – | **violation: never called the search API, fetched raw GitHub directly** |
| A5 | C1 snappy T4 | yes | pass | 0.0413 | 549 | 4 | 43.3 | 1,203,400 | 3 | – | clean (`corpus add` → `index` → `code`) |
| A6 | C1 snappy T4 | yes | fail† | 0.0293 | 413 | 4 | 21.3 | 11,467,819 | 4 | no_corpus → wrong | clean; pulled snappy (11.3 MB); answered_from local_pack |
| A7 | C1 snappy T4 | yes | fail† | 0.0294 | 525 | 4 | 14.3 | 18,262 | 4 | no_corpus → wrong | clean; pack pre-pulled (12.0 s setup); answered_from local_pack |
| A0 | C2 lz4 S1 | yes | unscored‡ | 0.0161 | 633 | 1 | 7.6 | 16,878 | 0 | – | clean |
| A1 | C2 lz4 S1 | yes | unscored‡ | 0.0215 | 994 | 1 | 9.7 | 18,422 | 0 | – | note: answered with no tool call |
| A2 | C2 lz4 S1 | yes | unscored‡ | 0.0217 | 941 | 1 | 9.0 | 18,544 | 0 | – | note: answered with no tool call |
| A3 | C2 lz4 S1 | yes | unscored‡ | 0.0215 | 988 | 1 | 9.8 | 19,372 | 0 | – | note: answered with no tool call |
| A4 | C2 lz4 S1 | yes | unscored‡ | 0.0200 | 850 | 1 | 9.4 | 17,739 | 0 | – | note: answered with no tool call |
| A5 | C2 lz4 S1 | yes | unscored‡ | 0.0527 | 1054 | 4 | 37.2 | 488,952 | 3 | – | clean |
| A6 | C2 lz4 S1 | yes | unscored‡ | 0.0316 | 898 | 3 | 31.3 | 4,700,587 | 2 | no_corpus → wrong | clean; pulled lz4 (4.6 MB); local_pack |
| A7 | C2 lz4 S1 | yes | unscored‡ | 0.0309 | 929 | 3 | 14.3 | 19,579 | 2 | no_corpus → wrong | clean; pre-pulled (15.3 s); local_pack |

\* Loaded machine: timings are invalid (PLAN §6). † The raw `check.py` grade was a
pass. PLAN H2 turns it into a fail because the first v2 question did not route (see
below). ‡ `ANTHROPIC_API_KEY` is not set on this host, so every C2 cell is `unscored`,
exactly as PLAN §6 requires.

### What the smoke run verified

- **Each arm had only its intended tools.** The init event's `tools` matches the
  declared set in every cell: A0 has none, A2 adds WebFetch/WebSearch, and the rest
  have Bash/Read/Grep/Glob. MCP servers and plugins: none. Model: `claude-sonnet-5-5`.
  An independent grep over all 16 raw transcripts found no `xerj` call outside A5–A7,
  no `git` outside A1, no clone or archive download in A2–A7, and only Sourcegraph in
  A3. The check caught one real case of contamination: the A4 C1 cell, which is marked
  invalid.
- **The v2 arms really used the pack.** Both A6 and A7 got `answered from: local pack`
  in every cell. A6 started cold (`packs_before = []`) and its pull shows up in the
  bytes (11,467,819 B against the 11,338,487 B snappy pack; 4,700,587 B against the
  4,623,814 B lz4 pack). A7 started warm (`packs_before = [snappy]` / `[lz4]`), with
  the pre-pull timed separately (`setup.rx` 11,472,275 / 4,671,184 B). Its downloads
  while the agent ran were only about 18 KB of baseline noise. No v2 arm touched
  `corpus add` or `corpus index`, and the v1 arm never touched `ask` or `hub`.
- **Metrics are captured** for every cell: correct (or unscored), cost and output tokens
  (summed over every `result` event, with ≥1 event asserted), turns, bytes downloaded
  (the jail interface delta minus Anthropic-range bytes from an nft counter), wall
  time with 1/5/15 load before and after, fetches and failed fetches, xerj calls,
  routing, A7 setup time and bytes. Schema: [`results/schema.json`](./results/schema.json).

### Product observations (seen while validating; NOT results, not measured)

These fall under H2 (routing). They are listed so nobody is surprised by them in the full run:

1. **The router returned NoCorpus for every first v2 question in the smoke run (4/4)**,
   for example `xerj "In snappy, what is the exact value of kInputMarginBytes?"` →
   `no hub corpus matches … closest corpora: snappy, lz4` (exit 1). Every agent then
   re-asked with `--corpus`. With only 3 corpora in the catalogue this is the sketch
   scoring (§5 step 3: tokens present / tokens total, ×0.8): ordinary question words
   dilute the score below 0.6.
2. **Def mode missed `kInputMarginBytes`**: `ask --corpus snappy "what is the exact
   value of kInputMarginBytes"` exited 1. It is a function-local `const` in
   `snappy.cc`, so it is not a def record. A plain how-query then found it.
3. **First use on a cold machine:** in a no-agent dry run at load ~50, the
   auto-started node did not report healthy within the 60 s wait, so the first `ask`
   failed. The second one worked. A6 may hit this on a loaded runner.

## Harness defects found and fixed

| # | Defect | Effect if unfixed | Fix |
|---|---|---|---|
| 1 | `sandbox.sh` shared the **host network**. `xerj ask` probes `localhost:9200` first and uses whatever answers. | On this host, :9200 is the user's reference-coding node, so a v2 arm would have restored packs **into the user's node** and answered from its corpora. That is both contamination and a write to a node we must never touch. An A5 agent's `corpus add/index` would have hit it too. | Private network namespace via `pasta` (`-t/-u/-T/-U none --no-map-gw`: nothing forwarded either way), plus an nft rule rejecting RFC1918/CGNAT/link-local destinations, which closes the route through `docker0` (10.200.0.1). Verified: a host server on 127.0.0.1 and on 0.0.0.0 is unreachable from the jail, while github.com and the API are reachable. |
| 2 | `--ro-bind / /` exposed all of `/home`. | Arms could read the gold answers (`benchmarks/corpus-tasks`, the 2026-10-13 run dir), the local corpus clones in `~/.xerj-code`, and `.env` secrets. | `/home` and `/root` are masked with tmpfs. Only HOME, the auth dir and explicit `--ro` paths are visible. |
| 3 | `CLAUDE_CONFIG_DIR` was bound as-is. | Every arm loaded the user's global `CLAUDE.md`, skills, plugins and Stop hook, and could read other sessions' transcripts and history. `settings.json` `env` **injected `GH_TOKEN` into every arm**, despite `--clearenv`. | Everything in the dir except `.credentials.json` and `.claude.json` is masked, and `settings.json` is replaced by `{}`. The harness also passes `--strict-mcp-config --mcp-config '{"mcpServers":{}}' --disable-slash-commands --tools <arm set>`. |
| 4 | Inside pasta's user namespace the jail ran as uid 0. | `claude --dangerously-skip-permissions` refuses to run as root. | `bwrap --unshare-user --uid/--gid <real ids>`. |
| 5 | The host `resolv.conf` points at 127.0.0.53 (host loopback). | No DNS in the jail. | The jail gets its own `resolv.conf` (1.1.1.1, 8.8.8.8). |
| 6 | Masking files by binding `/dev/null` read back as EACCES. | `CLAUDE.md` read errors. | Masks are bound from regular empty files. |
| 7 | Catalogue override: `build-catalogue` wants the `.sig` beside each pack and verifies it with the client key; `sig_url` points at the real-key release `.sig`. | The override could not be built with a throwaway key, and a pull would fail signature verification. | Packs are re-signed with the throwaway key, `sig_url` is rewritten to the in-jail server, the catalogue is re-signed, and `XERJ_HUB_PUBKEY` is set for `build-catalogue` and the v2 shim (see above). |
| 8 | A5 prompt said `xerj corpus index` without the corpus name. | One wasted turn per A5 cell. | `xerj corpus index <slug>`. |
| 9 | The bytes counter mis-parsed `/proc/net/dev`: interface lines with no leading space shift the columns, so it summed packets instead of bytes. | `bytes_downloaded` was always ≈0. | Fixed parser; the A7 pre-pull is measured separately (`setup.rx`). |
| 10 | The contamination check let an A4 agent skip its search API. | An A4 cell measured A2. | A3/A4 must call their endpoint (violation otherwise); tool arms that answered with zero tool calls get a note. |
| 11 | Clone bytes were measured from `.git` dirs in the work dir. | Agents clone into the jail's `/tmp`, so the measure read 0. | Removed. Clone bytes are in the network-namespace `bytes_downloaded`. |

## Ambiguities in PLAN.md (flagged, not silently chosen)

The harness had to pick something to run the smoke. Each item below is what it picked,
and needs the user's decision at freeze (PLAN §10):

1. **"Nine arms"** (§3), but the table lists eight (A0–A7). The harness implements the eight listed.
2. **A3/A4 endpoints** are "locked at freeze". Provisional: A3 = Sourcegraph public
   stream search (`sourcegraph.com/.api/search/stream`, unauthenticated; it searches
   upstream HEAD, not the pin); A4 = DuckDuckGo HTML (`html.duckduckgo.com/html/`, no
   key). grep.app was rejected because it answers curl with a Vercel bot checkpoint (429).
3. **`--max-turns`**: "same for every arm", value not given. Provisional: 15, with A0 = 1
   and sessions at 15 × questions.
4. **Pin vs "current development source"**: the C1 questions ask for "current" while
   §4 grades at the pack pin. The harness adds one line to every arm's prompt: "The
   reference version is commit `<sha>` (`<date>`) of `<url>`; answer for that
   version". A1 is told how to fetch that commit.
5. **A2 "may not git clone"**: also read as no archive/tarball download (a clone by
   other means). Single-file raw fetches are allowed. A `git` shim refuses
   clone/fetch/pull/ls-remote.
6. **A5 "install xerj"**: the binary is placed on PATH for A5–A7 alike, so install
   cost is not measured. The prior run instead had the agent read llms.txt and install.
7. **A7 "steady state"**: the harness pre-pulls in the same jail, so the hub node is
   already up when the agent starts. A6 pays node boot plus pull.
8. **H2 and NoCorpus**: H2 counts "Confident, or Ambiguous containing the correct
   slug" as correct. The client actually returned **NoCorpus with a "closest corpora"
   list that contained the right slug**. Literal reading: not ok, so the answer is
   counted wrong even when the agent re-asks with `--corpus` and gets it right. The
   harness does this and records `closest_contains_expected` so the decision can be
   revisited.
9. **Corpora**: §4 names lmdb, quinn, leveldb, boringtun, badger, s2n-tls and lz4.
   Only lz4/snappy/zstd have v2 packs today. **C3 cannot run at all**: no non-git,
   multi-source corpus has a v2 pack.
10. **C2/C3 tasks** must be authored by a person and committed before any C2/C3 run
    (§5). None exist. `tasks/C2/lz4.smoke.json` is a smoke fixture written by the
    harness agent, and `pov.py` refuses it without `--allow-smoke-fixtures`.
11. **Judge**: no `ANTHROPIC_API_KEY` on this host, so the judge's live HTTP path is
    untested (the unscored path is tested). Verify at freeze that `claude-opus-4-8`
    exists and accepts `temperature: 0`.
12. **Hosted**: spec §12 puts hosted answers out of scope, so A6's first use is always a
    synchronous pull, never a hosted answer plus a background pull (§1 describes both).
13. **"Quiet machine"**: this host ran at load 25–113. The full run needs a host where
    load stays low, or the wall-time column has to be dropped.
14. **Failed fetches** is a heuristic: the tool result is empty or an error, or
    matches captcha/checkpoint/403/429 text.
15. **C1 grading looseness** (registered suite, unchanged): regexes like `0xf|15` are
    unanchored, so for example "115" passes. This is pre-existing and noted because it
    affects C1 accuracy for every arm equally.
16. `rg` is not installed on this host. A1's "ripgrep" falls back to grep, and the
    prompt says so.

## Estimated cost and wall time of the full pre-registered run

Assumptions: the 3 available packs; C1 = 3 × 10 registered tasks; C2 = 3 × 5 authored
tasks (count not fixed in the PLAN); C3 = 0 (no pack); C4 = 3 sessions × 10 questions;
8 arms; 3 repeats; serial. Cost per question unit is the smoke per-arm mean (about
$0.015–0.047; A5 is the most expensive).

| | agents | judge (C2 + C4's C2-style, opus) | wall (serial) |
|---|---|---|---|
| 3 repeats | **≈ $45** | ≈ $20 | **≈ 8.5 h** |
| + "+2 repeats on CV > 0.5" for ~30% of cells | ≈ $59 | ≈ $25 | ≈ 11 h |

**This is a floor, not a quote.** The smoke questions were easy: answered in 1–4
turns, and most C2 cells used no tools at all. Authored cross-file C2 questions and
10-question C4 sessions will take more turns. If the pinned agent model is Opus
rather than Sonnet, the per-turn price is higher. A realistic envelope is
**$150–300 and 10–15 h**. Run one repeat of the frozen matrix first and re-estimate
from that before committing the rest.

## Running it

```sh
# once per run dir (downloads + verifies the packs, builds the override, writes shims)
python3 benchmarks/hub-pov/harness/pov.py prepare --xerj engine/target/release/xerj \
    --registry <corpus-hub checkout>/tools/xerj-code/hub --registry-commit <sha>
python3 benchmarks/hub-pov/harness/pov.py verify-c1          # vendored C1 suites == bench/corpus-tasks
python3 benchmarks/hub-pov/harness/pov.py matrix benchmarks/hub-pov/matrix/smoke.json --run-id <id>
python3 benchmarks/hub-pov/harness/pov.py score  --run-id <id>
python3 benchmarks/hub-pov/harness/pov.py report --run-id <id>
python3 -m unittest benchmarks/hub-pov/harness/test_pov.py   # offline parser/grader/contamination tests
```

Results are written under `$POV_RUN_DIR` (default `/home/claude/hubdev-wt/pov-run`) and
never into the repository. Nothing in the harness publishes anything.
