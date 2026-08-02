//! Issue #122: Painless had no per-evaluation CPU budget.
//!
//! Every pre-existing limit bounds a *structural* property of a script — how
//! deep it nests (`MAX_PARSE_DEPTH`, `MAX_EVAL_DEPTH`), how many closure calls
//! it makes (`MAX_CALL_DEPTH`, `MAX_CALL_COUNT`), how many bytes of source it
//! is (`MAX_SCRIPT_LEN`). None of them bounds *work*, so a script can be flat,
//! shallow, closure-free and comfortably under 64 KiB while costing over a
//! second of CPU on a single document. The doc-scan cooperative timeout poll
//! cannot help: it only decides how often a document *boundary* is checked.
//!
//! The measurement entry points below are `#[ignore]`d — they are stopwatches,
//! not assertions, and are the numbers quoted in the fix's commit message. The
//! non-ignored tests are the regression: they fail on the pre-fix tree by
//! running to completion (or by taking a wall-clock eternity) instead of being
//! refused.

use serde_json::{json, Value};
use xerj_engine::painless::{eval_painless, is_resource_limit_error, PainlessCtx};

/// Bytes of string literal the adversarial script starts from.
const CHUNK: usize = 1024;

/// A flat, closure-free, legal-size script whose cost is quadratic in its own
/// statement count: statement *i* concatenates a 1 KiB chunk onto an
/// accumulator that is already `i` KiB long, so `n` statements copy
/// `~n²/2 KiB`. Nothing about it is deep, recursive or oversized — it is the
/// counter-example to every structural limit at once.
fn quadratic_concat_script(statements: usize) -> String {
    let mut src = String::with_capacity(CHUNK + statements * 10 + 64);
    src.push_str("def c = \"");
    src.push_str(&"x".repeat(CHUNK));
    src.push_str("\";\ndef s = c;\n");
    for _ in 0..statements {
        src.push_str("s = s + c;\n");
    }
    src.push_str("return s.length();");
    src
}

/// The benign comparison point: a legal script of comparable *size* that does
/// only cheap arithmetic, so any per-op overhead the budget adds shows up here
/// undiluted.
fn benign_arithmetic_script(terms: usize) -> String {
    let mut src = String::with_capacity(terms * 24 + 32);
    src.push_str("double t = 0;\n");
    for i in 0..terms {
        src.push_str(&format!("t = t + {}.5;\n", i % 97));
    }
    src.push_str("return t;");
    src
}

fn doc() -> Value {
    json!({ "rank": 7, "tags": ["a", "b"] })
}

fn params() -> Value {
    json!({ "factor": 2.0 })
}

/// One evaluation of `src`, returning the elapsed wall time and the outcome.
fn timed_eval(src: &str) -> (std::time::Duration, Result<(), String>) {
    let doc = doc();
    let params = params();
    let ctx = PainlessCtx::new(&doc, &params, 1.0);
    let start = std::time::Instant::now();
    let out = eval_painless(src, &ctx);
    let elapsed = start.elapsed();
    (elapsed, out.map(|_| ()))
}

/// Stopwatch (issue evidence #1 and #2): how long ONE document's evaluation of
/// the adversarial script takes. On the pre-fix tree this runs to completion
/// and prints the unbounded cost; with the budget in place it is refused.
///
/// `cargo test --release -p xerj-engine --test painless_cpu_budget -- \
///  --ignored --nocapture measure_adversarial`
#[test]
#[ignore = "stopwatch, not an assertion"]
fn measure_adversarial_cost_per_document() {
    // 5,700 statements is the largest this shape fits under the 64 KiB
    // `MAX_SCRIPT_LEN`, i.e. the worst case a caller is allowed to submit.
    for statements in [1000, 2000, 4000, 5700] {
        let src = quadratic_concat_script(statements);
        let (elapsed, outcome) = timed_eval(&src);
        println!(
            "adversarial statements={statements} source={} bytes ({:.1} KiB) \
             elapsed={:?} outcome={}",
            src.len(),
            src.len() as f64 / 1024.0,
            elapsed,
            match &outcome {
                Ok(()) => "COMPLETED (unbounded)".to_string(),
                Err(e) => format!("refused: {e}"),
            }
        );
    }
}

/// Stopwatch (issue evidence #3): per-evaluation cost of an ordinary small
/// script, reported as ns per evaluation. Run pinned:
///
/// `taskset -c 3 cargo test --release -p xerj-engine --test painless_cpu_budget \
///  -- --ignored --nocapture measure_ordinary`
#[test]
#[ignore = "stopwatch, not an assertion"]
fn measure_ordinary_script_ns_per_eval() {
    let doc = doc();
    let params = params();
    for (label, src) in [
        (
            "tiny",
            "doc['rank'].value * params.factor + _score".to_string(),
        ),
        ("small-arith", benign_arithmetic_script(16)),
        ("64KiB-benign", benign_arithmetic_script(4300)),
    ] {
        // Warm-up.
        for _ in 0..64 {
            let ctx = PainlessCtx::new(&doc, &params, 1.0);
            let _ = eval_painless(&src, &ctx);
        }
        // Report the MINIMUM over many short trials, not the mean over one
        // long run. This host is shared, and a mean absorbs every scheduling
        // interruption that lands in the run — the before/after difference
        // being measured here is a few ns per work unit and disappears under
        // that. The minimum is the trial that ran least interrupted.
        let iterations = if src.len() > 4096 { 20 } else { 2_000 };
        let trials = 40;
        let mut best = f64::MAX;
        for _ in 0..trials {
            let start = std::time::Instant::now();
            for _ in 0..iterations {
                let ctx = PainlessCtx::new(&doc, &params, 1.0);
                let out = eval_painless(&src, &ctx);
                assert!(out.is_ok(), "{label} must evaluate: {out:?}");
            }
            let per_eval = start.elapsed().as_nanos() as f64 / iterations as f64;
            best = best.min(per_eval);
        }
        println!(
            "ordinary {label:<13} source={:>6} bytes best-of-{trials} \
             {best:>10.1} ns/eval",
            src.len(),
        );
    }
}

/// THE regression. A flat 44.0 KiB script that trips no structural limit —
/// measured at 2.80 s per document on the pre-fix tree — must be refused as a
/// resource-limit failure instead of being run to completion.
///
/// Without the source fix this test does not fail fast — it *passes the wrong
/// way* by completing the evaluation successfully, which is precisely the
/// defect. The assertion is on the refusal.
#[test]
fn quadratic_string_growth_is_refused_as_a_resource_limit() {
    let src = quadratic_concat_script(4000);
    assert!(
        src.len() < 64 * 1024,
        "the repro must stay under the 64 KiB source limit, got {}",
        src.len()
    );
    let (elapsed, outcome) = timed_eval(&src);
    let err = outcome.expect_err(
        "a flat, closure-free, legal-size script ran to completion with no \
         work budget — this is issue #122",
    );
    assert!(
        is_resource_limit_error(&err),
        "the work budget must classify as a resource limit so the API layer \
         answers 400 instead of degrading to a wrong score, got {err:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "the budget must abandon the evaluation part-way through ONE document, \
         took {elapsed:?}"
    );
}

/// The same shape reached through repeated whole-document clones rather than
/// string growth: `doc['x'].value` copies the entire source document on every
/// access, so a flat script with thousands of accesses is linear in
/// `statements × document size` with no structural limit in the way.
#[test]
fn repeated_whole_document_clones_are_refused_as_a_resource_limit() {
    let big: Vec<Value> = (0..20_000).map(|i| json!(format!("value-{i}"))).collect();
    let doc = json!({ "rank": 7, "blob": big });
    let params = params();
    let mut src = String::from("double t = 0;\n");
    for _ in 0..2000 {
        src.push_str("t = t + doc['rank'].value;\n");
    }
    src.push_str("return t;");
    assert!(src.len() < 64 * 1024, "repro must stay legal-size");

    let ctx = PainlessCtx::new(&doc, &params, 1.0);
    let err = eval_painless(&src, &ctx)
        .expect_err("2000 whole-document clones ran unbudgeted — issue #122");
    assert!(
        is_resource_limit_error(&err),
        "expected a resource-limit classification, got {err:?}"
    );
}

/// The other half of the contract: ordinary scripts must be nowhere near the
/// budget. A script that is *itself* 64 KiB of legal arithmetic — the largest
/// benign script the source-size limit admits — must still evaluate cleanly.
#[test]
fn a_full_size_benign_script_still_evaluates() {
    let src = benign_arithmetic_script(4300);
    assert!(
        (32 * 1024..64 * 1024).contains(&src.len()),
        "the benign script must be big enough to be a real test, got {} bytes",
        src.len()
    );
    let (_, outcome) = timed_eval(&src);
    outcome.expect("a 64 KiB benign arithmetic script must not trip the budget");
}
