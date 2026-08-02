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

/// A flat 44.0 KiB script that trips no structural limit must be refused as a
/// resource-limit failure instead of being run to completion.
///
/// Its *result size* is separately capped at 1 MiB by the
/// `MAX_PAINLESS_STRING_LEN` limit that arrived with #87, so on this base it no
/// longer runs for seconds — but the work budget is what prices the copying,
/// and it is the budget that must be named in the refusal.
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
        err.contains("work budget"),
        "the budget must price the copying before the 1 MiB string-size cap \
         notices the result, got {err:?}"
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

// ── The `params` root (issue #122, round 2) ──────────────────────────────────
//
// `eval_access_chain` resolves `params.x`, `params['x']` and `params['_source']`
// itself, without a preceding value, in a fast path at chain position 0. The
// first revision of this budget ended each of those fast paths in `continue`,
// jumping past the charge at the bottom of the loop — so every one of them ran
// `PainlessValue::from_json` over a caller-supplied subtree (or, for
// `_source`, over a clone of the whole document) for a flat two work units.
// The budget had a hole of exactly the shape it was built to close.

/// `params` whose single member `blob` is an array of `nodes` short strings.
/// Short on purpose: under 64 bytes each contributes nothing beyond its one
/// node to the work count, so the weight here is honestly `nodes`, not a
/// string-length artefact.
fn params_with_blob(nodes: usize) -> Value {
    let blob: Vec<Value> = (0..nodes).map(|i| json!(format!("v{i}"))).collect();
    json!({ "blob": blob })
}

/// A flat, closure-free script of bare `params.blob;` statements. It nests
/// nothing, calls nothing, and at 3,000 statements is 36 KB — inside every
/// structural limit the interpreter has.
fn bare_params_read_script(statements: usize) -> String {
    let mut src = String::with_capacity(statements * 13 + 16);
    for _ in 0..statements {
        src.push_str("params.blob;\n");
    }
    src.push_str("return 1;");
    src
}

/// Stopwatch: cost of one document's evaluation of the bare-`params` script.
/// On the pre-fix tree this completes, and the two `nodes` rows show the cost
/// scaling linearly with the size of the caller's own `params`.
///
/// `taskset -c 3 cargo test --release -p xerj-engine --test painless_cpu_budget \
///  -- --ignored --nocapture measure_bare_params`
#[test]
#[ignore = "stopwatch, not an assertion"]
fn measure_bare_params_read_cost() {
    for nodes in [50_000, 200_000] {
        let params = params_with_blob(nodes);
        let doc = doc();
        let src = bare_params_read_script(3_000);
        let ctx = PainlessCtx::new(&doc, &params, 1.0);
        let start = std::time::Instant::now();
        let outcome = eval_painless(&src, &ctx);
        let elapsed = start.elapsed();
        println!(
            "bare-params nodes={nodes} source={} bytes elapsed={:?} units={} outcome={}",
            src.len(),
            elapsed,
            ctx.work_units(),
            match &outcome {
                Ok(_) => "COMPLETED (unbounded)".to_string(),
                Err(e) => format!("refused: {e}"),
            }
        );
    }
}

/// THE round-2 regression. 3,000 bare `params.blob;` statements against a
/// 200,000-node `params` — measured at 12.1 s per document with the fast path
/// uncharged — must be refused.
///
/// Reverting only `painless.rs` and keeping this file makes it fail by
/// completing the evaluation, which is the defect.
#[test]
fn bare_params_reads_are_charged_for_what_they_materialise() {
    let params = params_with_blob(200_000);
    let doc = doc();
    let src = bare_params_read_script(3_000);
    assert!(src.len() < 64 * 1024, "the repro must stay legal-size");

    let ctx = PainlessCtx::new(&doc, &params, 1.0);
    let start = std::time::Instant::now();
    let err = eval_painless(&src, &ctx).expect_err(
        "3,000 reads of a 200,000-node `params` member ran to completion — the \
         position-0 fast path is materialising an unbounded value for two work \
         units",
    );
    let elapsed = start.elapsed();
    assert!(
        is_resource_limit_error(&err),
        "expected a resource-limit classification, got {err:?}"
    );
    // Deterministic half: the reads must have been PRICED. Uncharged, this
    // whole script costs 2,048 work units.
    assert!(
        ctx.work_units() > 1_000_000,
        "3,000 reads of a 200,000-node value were charged {} work units",
        ctx.work_units()
    );
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "the budget must abandon this part-way through ONE document, took {elapsed:?}"
    );
}

/// The `params['x']` spelling reaches a different fast-path arm than
/// `params.x`, and both used to `continue` past the charge.
///
/// The assertion is on the WORK COUNT, not on wall time. Left uncharged these
/// reads are still eventually caught by the wall-clock half of the budget —
/// after 15 s — so a test that only asserts "some resource limit tripped"
/// passes on the broken tree. What has to be true is that the materialisation
/// was *priced*: on the pre-fix tree this whole script is charged 2,048 units,
/// which is the hole.
#[test]
fn subscripted_params_reads_are_charged_too() {
    let params = params_with_blob(200_000);
    let doc = doc();
    let mut src = String::new();
    for _ in 0..3_000 {
        src.push_str("params['blob'];\n");
    }
    src.push_str("return 1;");
    assert!(src.len() < 64 * 1024, "the repro must stay legal-size");

    let ctx = PainlessCtx::new(&doc, &params, 1.0);
    let err = eval_painless(&src, &ctx)
        .expect_err("3,000 subscripted reads of a 200,000-node `params` member ran unbudgeted");
    assert!(
        is_resource_limit_error(&err),
        "expected a resource-limit classification, got {err:?}"
    );
    assert!(
        ctx.work_units() > 1_000_000,
        "3,000 reads of a 200,000-node value were charged {} work units",
        ctx.work_units()
    );
}

/// `params['_source']` clones the whole document behind an O(1)-looking
/// expression. The fix's own commit message claimed this was already charged;
/// it was not, for exactly this single-step form.
#[test]
fn params_source_reads_are_charged_for_the_document_they_clone() {
    let big: Vec<Value> = (0..100_000).map(|i| json!(format!("v{i}"))).collect();
    let doc = json!({ "rank": 7, "blob": big });
    let params = json!({});
    let mut src = String::new();
    for _ in 0..3_000 {
        src.push_str("params['_source'];\n");
    }
    src.push_str("return 1;");
    assert!(src.len() < 64 * 1024, "the repro must stay legal-size");

    let ctx = PainlessCtx::new(&doc, &params, 1.0);
    let err = eval_painless(&src, &ctx)
        .expect_err("3,000 whole-document clones through `params['_source']` ran unbudgeted");
    assert!(
        is_resource_limit_error(&err),
        "expected a resource-limit classification, got {err:?}"
    );
    assert!(
        ctx.work_units() > 1_000_000,
        "3,000 whole-document clones were charged {} work units",
        ctx.work_units()
    );
}

/// A `params` small enough to be ordinary must still be readable thousands of
/// times — the charge prices what is materialised, it does not tax the access.
#[test]
fn ordinary_params_reads_are_not_refused() {
    let params = json!({ "factor": 2.0, "tags": ["a", "b", "c"] });
    let doc = doc();
    let mut src = String::from("double t = 0;\n");
    for _ in 0..2_000 {
        src.push_str("t = t + params.factor;\n");
    }
    src.push_str("return t;");
    let ctx = PainlessCtx::new(&doc, &params, 1.0);
    eval_painless(&src, &ctx).expect("2,000 reads of a 2-member params must not trip the budget");
}

// ── The wall-clock half of the budget ────────────────────────────────────────

/// Stopwatch: how far past its 100 ms slice an evaluation actually runs.
///
/// This is the number the `OPS_PER_CLOCK_CHECK` doc comment quotes. The
/// overshoot is one sampling window plus the step in flight, and a window is
/// bounded in work units, not in time — so it is worth measuring rather than
/// deriving from an average cost per unit.
///
/// `taskset -c 3 cargo test --release -p xerj-engine --test painless_cpu_budget \
///  -- --ignored --nocapture measure_time_budget`
#[tokio::test]
#[ignore = "stopwatch, not an assertion"]
async fn measure_time_budget_overshoot() {
    for doc_nodes in [10_000usize, 40_000, 160_000] {
        let big: Vec<Value> = (0..doc_nodes)
            .map(|i| json!(format!("value-{i}")))
            .collect();
        let doc = json!({ "rank": 7, "blob": big });
        let params = params();
        let mut src = String::from("double t = 0;\n");
        for _ in 0..2_000 {
            src.push_str("t = t + doc['rank'].value;\n");
        }
        src.push_str("return t;");

        let deadline = std::time::Instant::now() - std::time::Duration::from_secs(5);
        let (outcome, elapsed, units) =
            xerj_engine::painless::with_script_deadline(deadline, async {
                let ctx = PainlessCtx::new(&doc, &params, 1.0);
                let start = std::time::Instant::now();
                let out = eval_painless(&src, &ctx);
                (out, start.elapsed(), ctx.work_units())
            })
            .await;
        println!(
            "time-budget doc_nodes={doc_nodes} slice=100ms elapsed={elapsed:?} \
             overshoot={:?} units={units} outcome={}",
            elapsed.saturating_sub(std::time::Duration::from_millis(100)),
            match &outcome {
                Ok(_) => "COMPLETED".to_string(),
                Err(e) => format!("refused: {e}"),
            }
        );
    }
}

/// The TIME limit, tripped on its own.
///
/// Every other test here trips the deterministic op ceiling first, because
/// outside a request scope an evaluation is granted `MAX_EVAL_SLICE` (500 ms)
/// and 5,000,000 units of the cheapest work cost less than that. Inside a
/// request whose deadline has already passed — the normal state of a slow
/// search, since the doc scan only polls at document boundaries — the slice is
/// the `MIN_EVAL_SLICE` floor of 100 ms, and a shape whose real cost per work
/// unit is high reaches 100 ms of wall time long before 5,000,000 units.
///
/// `doc['x'].value` is that shape: it is charged one unit per document node,
/// but each node it copies is a heap allocation, so it costs far more per unit
/// than the byte-copying the counter is calibrated on.
#[tokio::test]
async fn the_time_budget_trips_on_its_own_inside_an_expired_request() {
    let big: Vec<Value> = (0..40_000).map(|i| json!(format!("value-{i}"))).collect();
    let doc = json!({ "rank": 7, "blob": big });
    let params = params();
    let mut src = String::from("double t = 0;\n");
    for _ in 0..2_000 {
        src.push_str("t = t + doc['rank'].value;\n");
    }
    src.push_str("return t;");
    assert!(src.len() < 64 * 1024, "the repro must stay legal-size");

    // A deadline already in the past: the request is over, the scan has not
    // noticed yet, and this evaluation gets the 100 ms floor.
    let deadline = std::time::Instant::now() - std::time::Duration::from_secs(5);
    let (err, elapsed) = xerj_engine::painless::with_script_deadline(deadline, async {
        let ctx = PainlessCtx::new(&doc, &params, 1.0);
        let start = std::time::Instant::now();
        let out = eval_painless(&src, &ctx);
        (
            out.expect_err("the time budget must abandon this"),
            start.elapsed(),
        )
    })
    .await;

    assert!(
        err.contains("time budget"),
        "the WALL-CLOCK half of the budget must be what trips, not the op \
         ceiling — got {err:?} after {elapsed:?}"
    );
    assert!(
        is_resource_limit_error(&err),
        "a time trip must classify as a resource limit, got {err:?}"
    );
    // The slice is 100 ms and the deadline is fixed before any work is
    // charged, so the overshoot is one sampling window plus the step in
    // flight — not a second 100 ms slice, which is what deriving the deadline
    // inside the first clock check used to cost.
    assert!(
        elapsed < std::time::Duration::from_millis(400),
        "a 100 ms slice was overshot to {elapsed:?}"
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
