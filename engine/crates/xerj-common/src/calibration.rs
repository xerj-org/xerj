//! Probability calibration for the decide surface — isotonic (PAVA) and
//! temperature (Platt) scaling, the reliability curve, and ECE (#1063).
//!
//! A probability from the decide ladder — the history vote's share, the local
//! head's softmax, and (when that tier exists) a hosted passthrough score — is
//! a *ranking* signal before it is an *odds*. The FiQA rerank measurement made
//! that concrete: provider noul probabilities with mean confidence 0.93 on a
//! bin whose actual relevance rate was 0.34, ECE 0.3109
//! (`benchmarks/beir-hybrid/results/2026-09-20-rerank-full-fiqa`). Ordering
//! worth +0.126 nDCG; a number that must never gate. This module is the
//! correction layer: one calibrated probability (`p_cal`) beside every raw one
//! (`p_raw`), never replacing it, and one ECE implementation reused by every
//! path that ships a probability.
//!
//! # The two methods, and their citations
//!
//! - **Isotonic** — non-decreasing regression of the empirical positive rate
//!   on the raw probability, by the pool-adjacent-violators algorithm (PAVA:
//!   Barlow, Bartholomew, Bremner & Brunk, *Statistical Inference under Order
//!   Restrictions*, Wiley 1972; the stack-of-blocks formulation is de Leeuw,
//!   Hornik & Mair, "Isotonic Optimization in R: Pool-Adjacent-Violators
//!   (PAVA) and Active Set Methods", JSS 2009). Its use for classifier scores
//!   is Zadrozny & Elkan, "Transforming classifier scores into accurate
//!   multiclass probability estimates", KDD 2002. Fitted values are joined by
//!   linear interpolation and clamped at the ends (constant extrapolation) —
//!   the same out-of-sample behaviour as scikit-learn's `IsotonicRegression`.
//! - **Temperature** — one scalar on the log-odds, `p' = σ(logit(p)/T)`,
//!   fitted by minimising held-out negative log-likelihood: Platt,
//!   "Probabilistic Outputs for Support Vector Machines", 1999 (the
//!   two-parameter original; the one-parameter scaling on the model's own
//!   log-odds is the form Guo, Pleiss, Sun & Weinberger, "On Calibration of
//!   Modern Neural Networks", ICML 2017, validate). Temperature is
//!   rank-preserving by construction: `σ(logit(·)/T)` is strictly increasing
//!   for any `T > 0`.
//!
//! # ECE
//!
//! Expected calibration error, the statistic the FiQA baseline is quoted in:
//! `ECE = Σ_b (n_b/N)·|acc_b − conf_b|` over equal-width bins of the
//! probability range (Naeini, Cooper & Hausknecht, "Obtaining Well Calibrated
//! Probabilities Using Bayesian Binning", AAAI 2015). Implemented once, in
//! [`ece_from_curve`], and reused for raw and calibrated probabilities alike.
//!
//! # Determinism (#940's lesson)
//!
//! Every fit here is a pure function of the pair multiset: no clock, no RNG,
//! no map-iteration order. The held-out split is a fixed rule — pairs
//! stable-sorted by `(p, y)`, every fifth held out — so the same history
//! produces bit-identical `p_cal` on every request, and the published
//! reliability curve is reproducible. Nothing in this module reads the
//! environment, the filesystem, or the system clock.

use serde::{Deserialize, Serialize};

/// Equal-width bins of the probability range used for reliability curves and
/// ECE — the count the published FiQA curve uses (10).
pub const CALIBRATION_BINS: usize = 10;

/// One labelled pair: a raw probability `p ∈ [0,1]` and its outcome `y ∈
/// {0,1}` (stored as `f64` so weighted/aggregate points — a bin's mean
/// confidence and positive rate — go through the same arithmetic).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CalibrationPair {
    pub p: f64,
    pub y: f64,
}

impl CalibrationPair {
    pub fn new(p: f64, y: bool) -> Self {
        Self {
            p,
            y: if y { 1.0 } else { 0.0 },
        }
    }
}

/// One bin of the reliability curve: the raw-probability interval `[lo, hi)`,
/// how many pairs fell in it, their mean raw probability (`mean_p`), and their
/// empirical positive rate (`positive_rate`). Serialize is derived so the
/// curve publishes verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ReliabilityBin {
    /// Inclusive lower edge of the bin.
    pub lo: f64,
    /// Exclusive upper edge; the last bin's edge is inclusive of 1.0.
    pub hi: f64,
    /// Pairs in this bin.
    pub n: usize,
    /// Mean raw (or, where the caller calibrated first, calibrated)
    /// probability of those pairs.
    pub mean_p: f64,
    /// Fraction of those pairs whose outcome was positive.
    pub positive_rate: f64,
}

/// Bin the pairs into `bins` equal-width bins of `[0,1]` and return the
/// non-empty ones, in bin order. Pure: the bin of a pair is a function of its
/// `p` alone (`min(floor(p·bins), bins−1)`), so the curve is deterministic
/// for a pair multiset regardless of input order.
pub fn reliability_curve(pairs: &[CalibrationPair], bins: usize) -> Vec<ReliabilityBin> {
    debug_assert!(bins > 0);
    let bins = bins.max(1);
    // (count, Σp, Σy) per bin — usize counts, f64 sums, no allocation for
    // empty bins beyond the three slots.
    let mut count = vec![0usize; bins];
    let mut sum_p = vec![0f64; bins];
    let mut sum_y = vec![0f64; bins];
    for pair in pairs {
        // A p outside [0,1] is the caller's bug, not a bin; clamp it into the
        // range rather than panicking on the index math below.
        let p = pair.p.clamp(0.0, 1.0);
        let b = ((p * bins as f64) as usize).min(bins - 1);
        count[b] += 1;
        sum_p[b] += p;
        sum_y[b] += pair.y;
    }
    (0..bins)
        .filter(|&b| count[b] > 0)
        .map(|b| {
            let n = count[b];
            ReliabilityBin {
                lo: b as f64 / bins as f64,
                hi: (b + 1) as f64 / bins as f64,
                n,
                mean_p: sum_p[b] / n as f64,
                positive_rate: sum_y[b] / n as f64,
            }
        })
        .collect()
}

/// ECE from a reliability curve: `Σ_b (n_b/N)·|positive_rate_b − mean_p_b|`.
/// The one implementation every probability-shipping path reuses — raw and
/// calibrated curves, the endpoint publication, and the tests that pin the
/// FiQA arithmetic. An empty curve has no probabilities to be wrong about: 0.
pub fn ece_from_curve(curve: &[ReliabilityBin]) -> f64 {
    let total: usize = curve.iter().map(|b| b.n).sum();
    if total == 0 {
        return 0.0;
    }
    curve
        .iter()
        .map(|b| (b.n as f64 / total as f64) * (b.positive_rate - b.mean_p).abs())
        .sum()
}

/// ECE of raw probabilities against their outcomes, binned — convenience over
/// [`reliability_curve`] + [`ece_from_curve`].
pub fn ece(pairs: &[CalibrationPair], bins: usize) -> f64 {
    ece_from_curve(&reliability_curve(pairs, bins))
}

// ─────────────────────────────────────────────────────────────────────────────
// Isotonic — PAVA
// ─────────────────────────────────────────────────────────────────────────────

/// An isotonic calibration fitted by PAVA: non-decreasing knots
/// `(x = p_raw, y = fitted probability)`, applied by linear interpolation
/// between neighbours and clamped at the ends.
///
/// Monotone by construction — the fit cannot invent a region where a higher
/// raw probability means a lower calibrated one — and it is the method that
/// meets the FiQA held-out gate (see
/// `benchmarks/decisions-calibration/`); temperature is the cheaper,
/// one-parameter alternative offered for the same job.
#[derive(Debug, Clone, PartialEq)]
pub struct IsotonicCalibration {
    /// Strictly increasing raw probabilities (tied `p` pooled into one
    /// weighted point at fit time).
    xs: Vec<f64>,
    /// The PAVA-fitted value at each `x` — non-decreasing.
    ys: Vec<f64>,
}

impl IsotonicCalibration {
    /// Fit on the pairs. Pure: ties in `p` are pooled by weighted mean and
    /// the PAVA pass is a deterministic stack algorithm, so the fit depends
    /// only on the pair multiset.
    pub fn fit(pairs: &[CalibrationPair]) -> Self {
        // Pool tied p: isotonic regression estimates a FUNCTION of p, so all
        // pairs at one p must share one fitted value — the weighted mean of
        // their outcomes. (A pair at the same p with outcomes 0 and 1 is not
        // a violation to pool; it is one point at the base rate.)
        let mut pooled: Vec<(f64 /*x*/, f64 /*Σy*/, f64 /*n*/)> = Vec::new();
        let mut sorted: Vec<CalibrationPair> = pairs.to_vec();
        sorted.sort_by(|a, b| {
            a.p.partial_cmp(&b.p)
                .unwrap()
                .then(a.y.partial_cmp(&b.y).unwrap())
        });
        for pair in sorted {
            match pooled.last_mut() {
                Some((x, sum_y, n)) if *x == pair.p => {
                    *sum_y += pair.y;
                    *n += 1.0;
                }
                _ => pooled.push((pair.p, pair.y, 1.0)),
            }
        }
        // PAVA (de Leeuw–Hornik–Mair's stack of blocks): walk the pooled
        // points in x order; while the previous block's weighted mean exceeds
        // the current one's, pool them. Each block is (Σy, n) over a
        // contiguous run of points; its mean is the fitted value for every
        // point in it.
        let mut blocks: Vec<(f64, f64, usize /*first pooled index*/)> = Vec::new();
        for (i, (_, sum_y, n)) in pooled.iter().enumerate() {
            blocks.push((*sum_y, *n, i));
            while blocks.len() >= 2 {
                let (prev, cur) = (blocks.len() - 2, blocks.len() - 1);
                let prev_mean = blocks[prev].0 / blocks[prev].1;
                let cur_mean = blocks[cur].0 / blocks[cur].1;
                if prev_mean > cur_mean {
                    let (sy, sn, _) = blocks.pop().expect("cur block");
                    let b = blocks.last_mut().expect("prev block");
                    b.0 += sy;
                    b.1 += sn;
                } else {
                    break;
                }
            }
        }
        // Expand blocks back to pooled points: every point in a block carries
        // the block's mean, and those (x, mean) pairs are the interpolation
        // knots.
        let mut ys = vec![0f64; pooled.len()];
        for (bi, (sum_y, n, first)) in blocks.iter().enumerate() {
            let mean = sum_y / n;
            let last = blocks.get(bi + 1).map(|b| b.2).unwrap_or(pooled.len());
            for y in &mut ys[*first..last] {
                *y = mean;
            }
        }
        Self {
            xs: pooled.iter().map(|(x, _, _)| *x).collect(),
            ys,
        }
    }

    /// The fitted knots, `(p_raw, p_cal)` — the parameters the
    /// `/_decide/_calibration` endpoint publishes.
    pub fn knots(&self) -> Vec<(f64, f64)> {
        self.xs
            .iter()
            .copied()
            .zip(self.ys.iter().copied())
            .collect()
    }

    /// Linear interpolation between knots; constant (clamped) outside their
    /// range. Monotone non-decreasing for any input, including inputs the fit
    /// never saw above or below its support.
    pub fn apply(&self, p: f64) -> f64 {
        match self.xs.first() {
            None => return p.clamp(0.0, 1.0), // empty fit: identity, caller gates on fit size
            Some(&first_x) if p <= first_x => return self.ys[0],
            _ => {}
        }
        if p >= *self.xs.last().expect("non-empty") {
            return *self.ys.last().expect("non-empty");
        }
        // Binary search for the knot interval containing p.
        let i = self.xs.partition_point(|&x| x < p);
        let (x0, y0) = (self.xs[i - 1], self.ys[i - 1]);
        let (x1, y1) = (self.xs[i], self.ys[i]);
        if x1 == x0 {
            return y0;
        }
        y0 + (p - x0) * (y1 - y0) / (x1 - x0)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Temperature — one scalar on the log-odds
// ─────────────────────────────────────────────────────────────────────────────

/// Probabilities at the rails have no finite log-odds; clamp before the
/// transform so 0 and 1 are representable (Platt's own practice).
const LOGIT_EPS: f64 = 1e-6;

fn logit(p: f64) -> f64 {
    let p = p.clamp(LOGIT_EPS, 1.0 - LOGIT_EPS);
    (p / (1.0 - p)).ln()
}

fn sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let e = z.exp();
        e / (1.0 + e)
    }
}

/// ln(1 + e^x), the numerically stable softplus — the two log terms of the
/// Bernoulli NLL.
fn softplus(x: f64) -> f64 {
    if x > 20.0 {
        x // e^-x underflows; ln(1+e^x) ≈ x to f64 precision
    } else if x < -20.0 {
        (-x).exp() // ≈ e^x, and ln(1+e^x) ≈ e^x
    } else {
        x.exp().ln_1p()
    }
}

/// A temperature calibration: `p_cal = σ(logit(p_raw)/T)`. `T > 1` flattens
/// confident probabilities toward the base rate, `T < 1` sharpens them; any
/// `T > 0` preserves rank exactly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TemperatureCalibration {
    pub t: f64,
}

/// The search bracket for `T`, on the log scale: a temperature outside
/// `[1/16, 16]` says the raw probabilities are unusable as odds in either
/// direction, which the caller reports rather than ships. A function, not a
/// const, because `f64::ln` is not const-callable.
fn t_bracket() -> (f64, f64) {
    ((1.0f64 / 16.0).ln(), 16.0f64.ln())
}

impl TemperatureCalibration {
    /// Fit `T` by minimising the pairs' negative log-likelihood — Platt
    /// (1999) / Guo et al. (2017)'s held-out form — by golden-section search
    /// on `ln T` over `t_bracket()`. Deterministic: a fixed bracket and a
    /// fixed 100 halvings of it, no starting point, no tolerance decision, so
    /// the same pairs always fit the same `T`.
    pub fn fit(pairs: &[CalibrationPair]) -> Self {
        let zs: Vec<f64> = pairs.iter().map(|p| logit(p.p)).collect();
        let nll = |ln_t: f64| -> f64 {
            let t = ln_t.exp();
            pairs
                .iter()
                .zip(&zs)
                .map(|(pair, &z)| {
                    let a = z / t;
                    pair.y * softplus(-a) + (1.0 - pair.y) * softplus(a)
                })
                .sum()
        };
        // Golden-section minimisation (the ratio φ−1 = 0.618…).
        let phi = (5f64.sqrt() - 1.0) / 2.0;
        let (mut lo, mut hi) = t_bracket();
        let mut c = hi - phi * (hi - lo);
        let mut d = lo + phi * (hi - lo);
        for _ in 0..100 {
            if nll(c) < nll(d) {
                hi = d;
            } else {
                lo = c;
            }
            c = hi - phi * (hi - lo);
            d = lo + phi * (hi - lo);
        }
        Self {
            t: ((lo + hi) / 2.0)
                .exp()
                .clamp(t_bracket().0.exp(), t_bracket().1.exp()),
        }
    }

    pub fn apply(&self, p: f64) -> f64 {
        sigmoid(logit(p) / self.t)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The fitted layer — method choice, held-out split, one report
// ─────────────────────────────────────────────────────────────────────────────

/// The configured calibration method — the `[decisions] calibration` setting's
/// type. `None` is the default: an uncalibrated node ships `p_raw` alone and
/// no `p_cal` at all, never a copy of `p_raw` pretending to be calibrated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CalibrationMethod {
    #[default]
    None,
    Isotonic,
    Temperature,
}

impl CalibrationMethod {
    /// The wire name — also the config-file value.
    pub fn as_str(self) -> &'static str {
        match self {
            CalibrationMethod::None => "none",
            CalibrationMethod::Isotonic => "isotonic",
            CalibrationMethod::Temperature => "temperature",
        }
    }
}

/// The fitted calibration, whichever method produced it. One `apply`, so the
/// caller never branches on the method to turn a raw probability into a
/// calibrated one — history-vote shares, local-head probabilities, and (when
/// the hosted tier is built) a passthrough score all go through this.
#[derive(Debug, Clone, PartialEq)]
pub enum FittedCalibration {
    Isotonic(IsotonicCalibration),
    Temperature(TemperatureCalibration),
}

impl FittedCalibration {
    /// The method's wire name (`isotonic` / `temperature`).
    pub fn method(&self) -> &'static str {
        match self {
            FittedCalibration::Isotonic(_) => "isotonic",
            FittedCalibration::Temperature(_) => "temperature",
        }
    }

    /// Calibrate one raw probability. Monotone non-decreasing for both
    /// methods, so a ranking by `p_raw` and by `p_cal` can disagree only on
    /// ties.
    pub fn apply(&self, p: f64) -> f64 {
        match self {
            FittedCalibration::Isotonic(f) => f.apply(p),
            FittedCalibration::Temperature(f) => f.apply(p),
        }
    }
}

/// The deterministic held-out split: every fifth pair, in `(p, y)` order, is
/// held out — 20 % of the history, stratified across the probability range
/// (so the held-out set spans the same `p_raw` support the fit sees, which is
/// what makes an isotonic fit evaluable), with no clock and no RNG. The same
/// multiset always splits identically (#940's lesson: a fit that moves
/// between requests is a defect, not a measurement).
pub fn held_out_split(pairs: &[CalibrationPair]) -> (Vec<CalibrationPair>, Vec<CalibrationPair>) {
    let mut sorted: Vec<CalibrationPair> = pairs.to_vec();
    sorted.sort_by(|a, b| {
        a.p.partial_cmp(&b.p)
            .unwrap()
            .then(a.y.partial_cmp(&b.y).unwrap())
    });
    let mut fit = Vec::with_capacity(sorted.len() * 4 / 5);
    let mut held = Vec::with_capacity(sorted.len() / 5 + 1);
    for (i, pair) in sorted.into_iter().enumerate() {
        if i % 5 == 4 {
            held.push(pair);
        } else {
            fit.push(pair);
        }
    }
    (fit, held)
}

/// The full fitted report `/_decide/_calibration` publishes: what was fitted
/// on, how it evaluates, and the reliability curves of the raw and calibrated
/// probabilities over every labelled pair.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationReport {
    /// The method that produced this fit.
    pub method: CalibrationMethod,
    /// The fit itself, for applying to new probabilities.
    pub fit: FittedCalibration,
    /// Pairs the fit was fitted on (80 %).
    pub fitted_on: usize,
    /// Pairs the ECEs below evaluate on (20 %, held out of the fit).
    pub held_out: usize,
    /// Every labelled pair the report saw (fit + held out).
    pub labelled_pairs: usize,
    /// Held-out ECE of the raw probabilities — what the node would ship
    /// without this layer.
    pub ece_raw: f64,
    /// Held-out ECE after calibration — the number shipped beside every
    /// `p_cal`.
    pub ece_calibrated: f64,
    /// Reliability curve of the raw probabilities over all pairs.
    pub curve_raw: Vec<ReliabilityBin>,
    /// Reliability curve of the calibrated probabilities over all pairs.
    pub curve_calibrated: Vec<ReliabilityBin>,
    /// The RFC 3339 range of the fitted documents' `ts` fields, when they
    /// carry one — publication metadata stamped by the caller that owns the
    /// documents, never an input to the fit itself (which stays a pure
    /// function of the pairs).
    pub ts_range: Option<(String, String)>,
}

/// Fit `method` on a held-out split of `pairs` and evaluate it: fit on 80 %,
/// publish raw and calibrated ECE on the 20 % held out, and the reliability
/// curves over all pairs. Pure — the whole report is a function of the pair
/// multiset. `None` fits nothing and reports nothing; the caller decides what
/// an unconfigured node ships (no `p_cal` at all).
pub fn fit_calibration(
    method: CalibrationMethod,
    pairs: &[CalibrationPair],
) -> Option<CalibrationReport> {
    if method == CalibrationMethod::None {
        return None;
    }
    let (fit_pairs, held_pairs) = held_out_split(pairs);
    let fit = match method {
        CalibrationMethod::Isotonic => {
            FittedCalibration::Isotonic(IsotonicCalibration::fit(&fit_pairs))
        }
        CalibrationMethod::Temperature => {
            FittedCalibration::Temperature(TemperatureCalibration::fit(&fit_pairs))
        }
        CalibrationMethod::None => return None,
    };
    let raw_pairs: Vec<CalibrationPair> = pairs.to_vec();
    let calibrated_pairs: Vec<CalibrationPair> = pairs
        .iter()
        .map(|p| CalibrationPair {
            p: fit.apply(p.p),
            y: p.y,
        })
        .collect();
    let held_raw: Vec<CalibrationPair> = held_pairs.clone();
    let held_calibrated: Vec<CalibrationPair> = held_pairs
        .iter()
        .map(|p| CalibrationPair {
            p: fit.apply(p.p),
            y: p.y,
        })
        .collect();
    Some(CalibrationReport {
        method,
        fit,
        fitted_on: fit_pairs.len(),
        held_out: held_pairs.len(),
        labelled_pairs: pairs.len(),
        ece_raw: ece(&held_raw, CALIBRATION_BINS),
        ece_calibrated: ece(&held_calibrated, CALIBRATION_BINS),
        curve_raw: reliability_curve(&raw_pairs, CALIBRATION_BINS),
        curve_calibrated: reliability_curve(&calibrated_pairs, CALIBRATION_BINS),
        ts_range: None,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests — the algorithms are the contract, so they are pinned directly
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(pts: &[(f64, f64)]) -> Vec<CalibrationPair> {
        pts.iter().map(|&(p, y)| CalibrationPair { p, y }).collect()
    }

    /// The FiQA baseline's own reliability curve, verbatim from
    /// `benchmarks/beir-hybrid/results/2026-09-20-rerank-full-fiqa/fiqa-full-run.json`
    /// (`calibration.curve`, bins 0–9): `(mean_conf, acc, n)`. 19,440
    /// (probability, relevance) pairs, published ECE 0.3109.
    const FIQA_BASELINE_CURVE: [(f64, f64, usize); 10] = [
        (0.0455, 0.0002, 5572),
        (0.1418, 0.0003, 3005),
        (0.2434, 0.0034, 2061),
        (0.3438, 0.0033, 1529),
        (0.4445, 0.0077, 1423),
        (0.5442, 0.0071, 1271),
        (0.6451, 0.0224, 1118),
        (0.7455, 0.0408, 1224),
        (0.8452, 0.1242, 1272),
        (0.9297, 0.3378, 965),
    ];

    /// Expand binned aggregates into pairs at the bin's mean confidence, the
    /// bin's count split by its positive rate — the reconstruction every
    /// FiQA number in this module works from (the run retained bins, not
    /// pairs).
    fn bin_pairs<'a>(
        bins: impl IntoIterator<Item = &'a (f64, f64, usize)>,
    ) -> Vec<CalibrationPair> {
        let mut out = Vec::new();
        for &(conf, acc, n) in bins {
            let positives = (acc * n as f64).round() as usize;
            for i in 0..n {
                out.push(CalibrationPair {
                    p: conf,
                    y: if i < positives { 1.0 } else { 0.0 },
                });
            }
        }
        out
    }

    fn fiqa_baseline_pairs() -> Vec<CalibrationPair> {
        bin_pairs(FIQA_BASELINE_CURVE.iter())
    }

    /// PAVA's defining property on the classic textbook input: the raw points
    /// dip mid-range, and the fit pools them into the weighted mean of the
    /// violating run.
    #[test]
    fn pava_pools_the_violating_run() {
        // (x, y) with equal weights: 0→0, 1→1, 2→0, 3→1 dips at x=2; the
        // block {1,2} violates and pools to 0.5 each.
        let pts = pairs(&[(0.0, 0.0), (0.3, 1.0), (0.6, 0.0), (0.9, 1.0)]);
        let fit = IsotonicCalibration::fit(&pts);
        let knots = fit.knots();
        assert_eq!(knots[1].1, 0.5, "the violating run pools: {knots:?}");
        assert_eq!(knots[2].1, 0.5, "both members of the run share one value");
        assert_eq!(knots[0].1, 0.0, "an undisrupted point is untouched");
        assert_eq!(knots[3].1, 1.0);
    }

    /// Isotonic output is monotone non-decreasing in the input, on data whose
    /// empirical rates are noisy — the property that makes p_cal a
    /// probability, not a re-scoring.
    #[test]
    fn isotonic_output_is_monotone() {
        let pts = pairs(&[
            (0.05, 0.0),
            (0.1, 1.0),
            (0.15, 0.0),
            (0.2, 0.0),
            (0.5, 1.0),
            (0.55, 0.0),
            (0.9, 1.0),
            (0.95, 0.0),
        ]);
        let fit = IsotonicCalibration::fit(&pts);
        let mut prev = 0.0f64;
        for p in [0.0, 0.01, 0.1, 0.3, 0.5, 0.7, 0.9, 0.99, 1.0] {
            let cal = fit.apply(p);
            assert!(
                cal >= prev - 1e-12,
                "apply({p}) = {cal} < apply of a smaller input {prev}"
            );
            assert!((0.0..=1.0).contains(&cal), "p_cal is a probability: {cal}");
            prev = cal;
        }
    }

    /// Interpolation between knots, clamped outside them: a fit on [0.2, 0.8]
    /// is constant below 0.2 and above 0.8, linear between.
    #[test]
    fn isotonic_interpolates_and_clamps() {
        let pts = pairs(&[(0.2, 0.0), (0.8, 1.0)]);
        let fit = IsotonicCalibration::fit(&pts);
        assert_eq!(fit.apply(0.0), 0.0, "below the lowest knot: clamped");
        assert_eq!(fit.apply(1.0), 1.0, "above the highest knot: clamped");
        assert!((fit.apply(0.5) - 0.5).abs() < 1e-12, "linear between knots");
    }

    /// Tied raw probabilities pool to their base rate — one p, one fitted
    /// value, whatever outcomes share it.
    #[test]
    fn tied_probabilities_pool() {
        let pts = pairs(&[(0.7, 1.0), (0.7, 0.0), (0.7, 0.0), (0.7, 1.0), (0.7, 0.0)]);
        let fit = IsotonicCalibration::fit(&pts);
        assert_eq!(fit.knots().len(), 1, "one distinct p → one knot");
        assert!((fit.apply(0.7) - 0.4).abs() < 1e-12, "the base rate 2/5");
    }

    /// Temperature preserves rank exactly and only moves the numbers: with
    /// T fit on data that is over-confident, every probability shrinks toward
    /// the middle without any two swapping order.
    #[test]
    fn temperature_shifts_but_preserves_rank() {
        let pts = pairs(&[
            (0.95, 0.0),
            (0.9, 0.0),
            (0.9, 1.0),
            (0.8, 0.0),
            (0.6, 0.0),
            (0.95, 1.0),
            (0.99, 1.0),
            (0.85, 0.0),
            (0.92, 0.0),
            (0.7, 0.0),
        ]);
        let fit = TemperatureCalibration::fit(&pts);
        assert!(fit.t > 1.0, "over-confident data fits T > 1: {}", fit.t);
        let raw = [0.1, 0.3, 0.5, 0.7, 0.85, 0.95, 0.99];
        let cal: Vec<f64> = raw.iter().map(|p| fit.apply(*p)).collect();
        let mut sorted_cal = cal.clone();
        sorted_cal.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(cal, sorted_cal, "rank preserved");
        assert!(cal[5] < 0.95, "high confidence shrinks: {}", cal[5]);
        assert!((fit.apply(0.5) - 0.5).abs() < 1e-9, "the midpoint is fixed");
    }

    /// Under-confident data fits T < 1 (sharpening) — the bracket's other
    /// half.
    #[test]
    fn under_confident_data_sharpens() {
        let pts = pairs(&[
            (0.6, 1.0),
            (0.62, 1.0),
            (0.58, 0.0),
            (0.61, 1.0),
            (0.4, 0.0),
            (0.39, 0.0),
            (0.41, 0.0),
            (0.6, 1.0),
            (0.42, 0.0),
            (0.59, 0.0),
        ]);
        let fit = TemperatureCalibration::fit(&pts);
        assert!(fit.t < 1.0, "under-confident data fits T < 1: {}", fit.t);
        assert!(fit.apply(0.61) > 0.61, "sharpened upward");
        assert!(fit.apply(0.41) < 0.41, "sharpened downward");
    }

    /// ECE identities: a perfectly calibrated set is 0, a constant confident
    /// wrong set is its full error, and the published FiQA ECE 0.3109 is
    /// reproduced by the shared implementation from the baseline's own curve
    /// — the arithmetic this module shares with the measurement that
    /// motivated it.
    #[test]
    fn ece_identities_and_the_fiqa_arithmetic() {
        // Perfect: every bin's mean equals its rate.
        let perfect: Vec<CalibrationPair> = (0..10)
            .flat_map(|b| {
                let p = (b as f64 + 0.5) / 10.0;
                let positives = (p * 10.0).round() as usize;
                (0..10).map(move |i| CalibrationPair {
                    p,
                    y: if i < positives { 1.0 } else { 0.0 },
                })
            })
            .collect();
        assert!(ece(&perfect, 10) < 0.11, "the discretisation is coarse");
        // Constant and confidently wrong: mean 0.9 against a 0.1 rate,
        // |0.9 − 0.1| = 0.8 exactly.
        let wrong: Vec<CalibrationPair> = (0..10)
            .map(|i| CalibrationPair {
                p: 0.9,
                y: (i == 0) as u8 as f64,
            })
            .collect();
        assert!((ece(&wrong, 10) - 0.8).abs() < 1e-12);
        // Empty: no probabilities, no error.
        assert_eq!(ece(&[], 10), 0.0);
        // The published FiQA ECE, recomputed from the baseline's own curve
        // (`fiqa-full-run.json`, calibration.curve) as pairs-at-the-bin-mean.
        let pairs = fiqa_baseline_pairs();
        assert_eq!(pairs.len(), 19_440);
        assert!(
            (ece(&pairs, 10) - 0.3109).abs() < 0.0005,
            "the FiQA ECE, from this module: {}",
            ece(&pairs, 10)
        );
    }

    /// The split is exactly 80/20, stratified across the probability range,
    /// and deterministic — the same multiset in a different order splits
    /// identically.
    #[test]
    fn held_out_split_is_deterministic_and_stratified() {
        let pts: Vec<CalibrationPair> = (0..100)
            .map(|i| CalibrationPair {
                p: (i % 10) as f64 / 10.0,
                y: (i % 3 == 0) as u8 as f64,
            })
            .collect();
        let (fit, held) = held_out_split(&pts);
        assert_eq!(fit.len(), 80);
        assert_eq!(held.len(), 20);
        // Stratified: every distinct p appears in both halves.
        for p in [0.0, 0.1, 0.5, 0.9] {
            assert!(fit.iter().any(|c| c.p == p), "p={p} in the fit set");
            assert!(held.iter().any(|c| c.p == p), "p={p} in the held-out set");
        }
        // Deterministic under input reordering: the same multiset splits to
        // the same two multisets (compare sorted, since the stable sort may
        // order equal (p, y) pairs differently in the two runs).
        let mut shuffled = pts.clone();
        shuffled.reverse();
        let (fit2, held2) = held_out_split(&shuffled);
        let canon = |mut v: Vec<CalibrationPair>| {
            v.sort_by(|a, b| {
                a.p.partial_cmp(&b.p)
                    .unwrap()
                    .then(a.y.partial_cmp(&b.y).unwrap())
            });
            v
        };
        assert_eq!(canon(fit), canon(fit2));
        assert_eq!(canon(held), canon(held2));
    }

    /// The issue's gate, on a deterministic bin-level reconstruction of the
    /// FiQA baseline: isotonic fitted on the EVEN bins, evaluated held-out on
    /// the ODD bins — whole probability regions the fit never saw, which is
    /// the honest form of held-out this data supports (the run retained bins,
    /// not pairs). This pins the same numbers the results file in
    /// `benchmarks/decisions-calibration/` publishes, so a regression in the
    /// fit code fails a test rather than a release gate.
    #[test]
    fn isotonic_meets_the_fiqa_gate_on_the_baseline_curve() {
        let fiqa = FIQA_BASELINE_CURVE;
        let fit_pairs: Vec<CalibrationPair> = bin_pairs(fiqa.iter().step_by(2));
        let held_pairs: Vec<CalibrationPair> = bin_pairs(fiqa.iter().skip(1).step_by(2));
        // Raw held-out ECE is the same order as the published 0.3109 —
        // the odd bins are representative, not a gift.
        let raw_held = ece(&held_pairs, 10);
        assert!(
            (raw_held - 0.3831).abs() < 0.0005,
            "raw held-out ECE {raw_held} (benchmarks/decisions-calibration)"
        );
        // Isotonic on the even bins, applied to the odd bins' means.
        let fit = IsotonicCalibration::fit(&fit_pairs);
        let calibrated: Vec<CalibrationPair> = held_pairs
            .iter()
            .map(|p| CalibrationPair {
                p: fit.apply(p.p),
                y: p.y,
            })
            .collect();
        let cal_held = ece(&calibrated, 10);
        // The exact value is pinned to the Python measurement
        // (benchmarks/decisions-calibration/fiqa_gate.py) — two independent
        // implementations of PAVA + ECE agreeing on the same split is a
        // cross-check on both.
        assert!(
            (cal_held - 0.0330).abs() < 0.0005,
            "held-out calibrated ECE {cal_held} (benchmarks/decisions-calibration says 0.0330)"
        );
        assert!(cal_held <= 0.10, "the #1063 gate");
    }

    /// An empty or single-point fit is degenerate but total: no panic, and
    /// apply stays in [0,1].
    #[test]
    fn degenerate_fits_are_total() {
        let empty = IsotonicCalibration::fit(&[]);
        assert_eq!(empty.apply(0.7), 0.7, "empty fit: identity");
        let single = IsotonicCalibration::fit(&[CalibrationPair { p: 0.6, y: 1.0 }]);
        assert!((single.apply(0.6) - 1.0).abs() < 1e-12);
        let temp = TemperatureCalibration::fit(&[CalibrationPair { p: 0.6, y: 1.0 }]);
        assert!(temp.t.is_finite() && temp.t > 0.0);
        assert!((0.0..=1.0).contains(&temp.apply(0.6)));
        // The rails do not produce infinities.
        assert!((0.0..=1.0).contains(&temp.apply(0.0)));
        assert!((0.0..=1.0).contains(&temp.apply(1.0)));
    }
}
