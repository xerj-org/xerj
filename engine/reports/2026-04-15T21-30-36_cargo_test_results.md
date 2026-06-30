# Cargo Test Results
**Date**: 2026-04-15 21:30:36 MDT
**Machine**: drai, 32 cores, 119Gi RAM
**xerj version**: 0.1.0
**Commit**: c7fa334 revert: M5.24 dv_cache LRU — didn't help either

## Summary
| Metric | Count |
|---|---:|
| **PASSED** | 192 |
| **FAILED** | 2 |
| **IGNORED** | 10 |

## Failures
- chaos_flush_during_writes
- chaos_rapid_restart_loop

### Failure details
```

thread 'chaos_flush_during_writes' (616108) panicked at crates/xerj-engine/tests/chaos_tests.rs:849:5:
assertion `left == right` failed: docs after restart mismatch
  left: 10000
 right: 5000
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- chaos_rapid_restart_loop stdout ----

┌─────────────────────────────────────────────────────────────────┐
│  [03:30:49Z]  Test 3: Rapid Restart Stability (100x restart loop) │
└─────────────────────────────────────────────────────────────────┘
  Seeded 500 docs (flushed to segment). Beginning 100 rapid restarts…
--

thread 'chaos_rapid_restart_loop' (616111) panicked at crates/xerj-engine/tests/chaos_tests.rs:549:5:
assertion `left == right` failed: rapid restart failed on 100 iterations
  left: 100
 right: 0


failures:
    chaos_flush_during_writes
    chaos_rapid_restart_loop

test result: FAILED. 8 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.44s

```

## Per-crate breakdown
```
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 53 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.51s
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 41 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 10 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: FAILED. 8 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.44s
```
