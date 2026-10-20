# Verified: the opener prompt works (2026-08-06)

The opener in `OPENER_PROMPT.md` was run **verbatim** as the instruction to a fresh
Claude Code agent, then independently verified (the verifier re-checked the
filesystem and XERJ, and compiled + tested the output — it did not trust the
executor's report).

That run used the wrapper scripts of the time (`xc-index.sh`, `xc.py`); they
were removed in #977, and the opener now names their built-in equivalents,
`xerj corpus index` and `xerj code`.

**Scenario.** A running XERJ at `:9200`, the `xerj-code` skill available, and one
reference library downloaded locally: `sift`, a real Rust crate whose API is
deliberately *not* `new()/push()/top_k()` but a two-phase, seal-gated count-min
sketch. The task: implement `pub fn top3(stream: &[u64]) -> Vec<(u64, u32)>`
returning the 3 heaviest keys — using sift's real API, which must be retrieved,
not guessed.

**What the opener drove Claude Code to do:**

1. **Index** the reference — `xc-index.sh refs --fresh` → 3 records live
   (`xc-refs-sift-src`, …); confirmed by `GET /_cat/indices` and `_count → 3`.
2. **Retrieve before writing** — `xc.py refs "sketch"` returned `sift/src/lib.rs`;
   the agent read the definition and used the real API.
3. **Write correct code** using `Sift::furnish(width, depth)` / `absorb` / `seal`
   / `crest` — including the `seal`-before-read contract.

**Independent verdict: WORKS (all four checks).**

- `indexed` ✓ · `retrieved` ✓ · `used_real_api` ✓ · `compiles_and_correct` ✓
- The verifier built a fresh cargo crate depending on the real sift path, dropped
  in the generated `solution.rs`, and ran `cargo test --release`:

  ```
  test tests::matches_crate_readme_shape ... ok
  test tests::heaviest_three ... ok
  test result: ok. 2 passed; 0 failed; 0 ignored
  ```
  asserting `top3([7,7,7,42,42,1,99]) == [(7,3),(42,2),(1,1)]` and
  `top3([5,5,5,5,8,8,8,3,3,1]) == [(5,4),(8,3),(3,2)]` — the exact ascending-key
  tiebreak sift documents.

**Why this is strong evidence retrieval was actually used, not bypassed:** sift's
defining feature is a seal-gated two-phase contract (read-before-seal panics,
absorb-after-seal panics, the crest is materialised *at* the seal) that
deliberately rejects the conventional `new/push/top_k` shape. A model working from
memory cannot invent this exact contract or the width-first/depth-second `furnish`
signature; code that respects it must have read the indexed source.

**The generated program (verbatim):**

```rust
use sift::Sift;

/// Return the 3 most frequent keys in `stream` as `(key, count)`, heaviest first.
pub fn top3(stream: &[u64]) -> Vec<(u64, u32)> {
    let mut sketch = Sift::furnish(4096, 4);   // width first, then depth
    for &key in stream {
        sketch.absorb(key);
    }
    sketch.seal();                             // mandatory before any read
    sketch.crest(3)
}
```
