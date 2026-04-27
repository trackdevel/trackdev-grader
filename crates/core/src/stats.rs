//! Small statistical helpers used across the analyze/ and process/ crates.
//!
//! Hand-rolled to avoid pulling in `statrs` / `ndarray`. The formulas match
//! the Python implementations byte-for-byte where the Python uses `statistics`
//! / custom arithmetic, and match `scipy.stats.linregress` (including its
//! Student's-t two-sided p-value) so trajectory classification agrees with
//! the Python reference when scipy is installed.

/// Arithmetic mean. Returns 0.0 on empty input.
pub fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// Population variance (divide by N, not N-1) — matches Python
/// `sum((v - mean) ** 2 for v) / len(v)` used throughout the codebase.
pub fn variance_pop(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let m = mean(values);
    values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / values.len() as f64
}

pub fn stddev_pop(values: &[f64]) -> f64 {
    variance_pop(values).sqrt()
}

/// Median via sort + mid-index lookup.
///
/// Matches Python's `sorted(values)[len(values) // 2]` semantics (even-length
/// lists return the upper of the two middle values, not their average) — this
/// is the form the Python reference uses.
pub fn median_upper(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

/// True median (average of the two middle values on even-length lists).
pub fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Python-style positional percentile: `sorted[len * p]` (truncating division).
/// Used by `_low_code_high_points` for the 25th percentile.
pub fn percentile_pos(values: &[f64], numerator: usize, denominator: usize) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = (v.len() * numerator) / denominator;
    v[idx.min(v.len() - 1)]
}

/// Median absolute deviation — used by the MAD-based outlier detection in
/// `repo_analysis::task_similarity`.
pub fn mad(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let m = median(values);
    let devs: Vec<f64> = values.iter().map(|v| (v - m).abs()).collect();
    median(&devs)
}

/// Gini coefficient. Returns 0.0 if all values are zero / empty.
pub fn gini(values: &[f64]) -> f64 {
    if values.is_empty() || values.iter().all(|v| *v == 0.0) {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len() as f64;
    let total: f64 = sorted.iter().sum();
    let cum: f64 = sorted
        .iter()
        .enumerate()
        .map(|(i, v)| (2.0 * (i as f64 + 1.0) - n - 1.0) * v)
        .sum();
    cum / (n * total)
}

/// Hoover (Robin Hood) index.
pub fn hoover(values: &[f64]) -> f64 {
    if values.is_empty() || values.iter().all(|v| *v == 0.0) {
        return 0.0;
    }
    let m = mean(values);
    let total: f64 = values.iter().sum();
    values.iter().map(|v| (v - m).abs()).sum::<f64>() / (2.0 * total)
}

/// Coefficient of variation = stddev / mean.
pub fn coefficient_of_variation(values: &[f64]) -> f64 {
    if values.is_empty() || values.iter().all(|v| *v == 0.0) {
        return 0.0;
    }
    let m = mean(values);
    if m == 0.0 {
        return 0.0;
    }
    stddev_pop(values) / m
}

/// Max / min ratio. Returns `f64::INFINITY` if min is 0 and max > 0.
pub fn max_min_ratio(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut mn = f64::INFINITY;
    let mut mx = f64::NEG_INFINITY;
    for &v in values {
        if v < mn {
            mn = v;
        }
        if v > mx {
            mx = v;
        }
    }
    if mn == 0.0 {
        return if mx > 0.0 { f64::INFINITY } else { 0.0 };
    }
    mx / mn
}

/// Simple linear regression (slope + r-squared) over `y` indexed by `0..n`.
/// Matches the Python manual fallback in `trajectory.py`; the p-value is
/// a crude chi-square-ish approximation used only for the threshold check.
pub struct LinregressResult {
    pub slope: f64,
    pub r_squared: f64,
    pub p_value: f64,
}

pub fn linregress_index(y: &[f64]) -> LinregressResult {
    let n = y.len();
    if n < 2 {
        return LinregressResult {
            slope: 0.0,
            r_squared: 0.0,
            p_value: 0.5,
        };
    }
    let n_f = n as f64;
    let x_mean = (n_f - 1.0) / 2.0;
    let y_mean = mean(y);

    let mut num = 0.0;
    let mut den = 0.0;
    for (i, &yi) in y.iter().enumerate() {
        let xi = i as f64;
        num += (xi - x_mean) * (yi - y_mean);
        den += (xi - x_mean).powi(2);
    }
    let slope = if den > 0.0 { num / den } else { 0.0 };

    let mut ss_res = 0.0;
    let mut ss_tot = 0.0;
    for (i, &yi) in y.iter().enumerate() {
        let xi = i as f64;
        let predicted = y_mean + slope * (xi - x_mean);
        ss_res += (yi - predicted).powi(2);
        ss_tot += (yi - y_mean).powi(2);
    }
    let r_squared = if ss_tot > 0.0 {
        1.0 - ss_res / ss_tot
    } else {
        0.0
    };

    // Two-sided p-value for H0: slope=0. Matches scipy.stats.linregress,
    // which drives p→0 whenever the fit is perfect (|r|=1): even with df=0
    // (n=2), scipy reports p=0 so the Python trajectory classifier fires
    // "growing" / "declining" on two-sprint series. ss_res is never exactly
    // zero after floating-point least-squares — a 2-point line leaves ~1e-34
    // residual — so compare against `ss_tot` with a relative epsilon.
    let p_value = if den <= 0.0 {
        0.5
    } else if ss_res <= 1e-24 * ss_tot {
        0.0
    } else if n <= 2 {
        0.5
    } else {
        let df = (n - 2) as f64;
        let se_slope = (ss_res / df / den).sqrt();
        if se_slope == 0.0 {
            0.0
        } else {
            let t = slope / se_slope;
            student_t_two_sided_p(t, df)
        }
    };

    LinregressResult {
        slope,
        r_squared,
        p_value,
    }
}

/// Two-sided p-value for Student's t with `df` degrees of freedom.
///
/// Uses the identity `2 * (1 - F(|t|; ν)) = I(ν/(ν+t²); ν/2, 1/2)` where `I`
/// is the regularized incomplete beta function. Matches
/// `scipy.stats.linregress().pvalue` for the n≥3 cases that trajectory
/// classification depends on.
pub fn student_t_two_sided_p(t: f64, df: f64) -> f64 {
    if df <= 0.0 || !t.is_finite() {
        return f64::NAN;
    }
    let x = df / (df + t * t);
    regularized_incomplete_beta(x, 0.5 * df, 0.5)
}

/// Regularized incomplete beta `I_x(a, b)`.
///
/// Uses the Gauss hypergeometric power series for `I_x(a, b)`, routing
/// through the symmetry `I_x(a, b) = 1 - I_{1-x}(b, a)` so the series is
/// always evaluated at the argument ≤ 0.5 where ratio terms shrink and
/// convergence is well-behaved. Accurate to ~1e-14 for all (a, b, x)
/// reachable from trajectory classification.
pub fn regularized_incomplete_beta(x: f64, a: f64, b: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    // Swap into the low-x branch so series converges fast.
    if x > (a + 1.0) / (a + b + 2.0) {
        return 1.0 - regularized_incomplete_beta(1.0 - x, b, a);
    }

    // front = x^a (1-x)^b / (a B(a, b)) in log space.
    let ln_front =
        ln_gamma(a + b) - ln_gamma(a + 1.0) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln();
    let front = ln_front.exp();

    // Σ_{n=0}^∞ C_n where C_0 = 1 and C_n = C_{n-1} · (a+b+n-1)/(a+n) · x.
    // Derived from ₂F₁(a+b, 1; a+1; x) = Σ ((a+b)_n / (a+1)_n) xⁿ — the
    // Gauss series used by Cephes' incbet.c when x·(a+b)/(a+1) < 1.
    const EPS: f64 = 1e-15;
    let mut term = 1.0_f64;
    let mut sum = 1.0_f64;
    for n in 1..1000 {
        let n_f = n as f64;
        term *= (a + b + n_f - 1.0) / (a + n_f) * x;
        sum += term;
        if term.abs() < EPS * sum.abs() {
            break;
        }
    }
    front * sum
}

/// Natural log of the gamma function via Lanczos approximation (Godfrey's
/// g=7 / n=9 coefficients). Relative error < 1e-14 for `x > 0`, which is
/// tight enough that `student_t_two_sided_p` matches `scipy.stats.t.sf` to
/// ~12 decimal places.
fn ln_gamma(x: f64) -> f64 {
    const G: f64 = 7.0;
    const COEFF: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.5203681218851,
        -1259.1392167224028,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507343278686905,
        -0.13857109526572012,
        9.984_369_578_019_572e-6,
        1.5056327351493116e-7,
    ];

    if x < 0.5 {
        // Reflection: Γ(x) Γ(1-x) = π / sin(π x).
        let sin_term = (std::f64::consts::PI * x).sin();
        return (std::f64::consts::PI / sin_term.abs()).ln() - ln_gamma(1.0 - x);
    }
    let z = x - 1.0;
    let mut sum = COEFF[0];
    for (i, &c) in COEFF.iter().enumerate().skip(1) {
        sum += c / (z + i as f64);
    }
    let t = z + G + 0.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (z + 0.5) * t.ln() - t + sum.ln()
}

/// Round to `ndigits` decimal places, matching Python `round(x, ndigits)`.
///
/// The naive `(x * 10^n).round_ties_even() / 10^n` diverges from Python at
/// values whose float representation is just under a halfway point: e.g.
/// `0.91355` is stored as `0.913549999…`, and Python's decimal-aware rounder
/// picks `0.9135`, but the multiply-then-round path lands on `9135.5`
/// exactly (f64 rounds the product up) and banker's-rounds to `0.9136`.
/// Routing through `format!("{:.N}")` — which uses round-half-to-even on the
/// exact shortest-decimal representation, same as Python — closes that gap.
pub fn round_half_even(x: f64, ndigits: u32) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{:.*}", ndigits as usize, x)
        .parse::<f64>()
        .unwrap_or(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gini_matches_known_values() {
        assert!((gini(&[1.0, 1.0, 1.0, 1.0]) - 0.0).abs() < 1e-9);
        // Perfect inequality (one takes all): (2n - n - 1) / (n * total) in the
        // Python-style formula, = (N-1)/N for N equal-zero except the last.
        let g = gini(&[0.0, 0.0, 0.0, 4.0]);
        assert!((g - 0.75).abs() < 1e-9, "got {g}");
    }

    #[test]
    fn hoover_symmetric_distribution_is_zero() {
        assert!(hoover(&[3.0, 3.0, 3.0]).abs() < 1e-9);
        // Two people, one has all: |0-5| + |10-5| = 10, total=10, hoover = 10/(2*10) = 0.5
        assert!((hoover(&[0.0, 10.0]) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn cv_constant_values() {
        assert!((coefficient_of_variation(&[5.0, 5.0, 5.0]) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn percentile_pos_matches_python_semantics() {
        // sorted[len//4] for 4 values = sorted[1]
        let p25 = percentile_pos(&[10.0, 4.0, 7.0, 1.0], 1, 4);
        assert!((p25 - 4.0).abs() < 1e-9);
    }

    #[test]
    fn median_upper_matches_python_floor_div() {
        // sorted[len//2] for 4 values = sorted[2]
        assert_eq!(median_upper(&[1.0, 2.0, 3.0, 4.0]), 3.0);
    }

    #[test]
    fn mad_simple() {
        // Values: [1,1,2,2,4,6,9]. Median = 2. Devs = [1,1,0,0,2,4,7]. MAD = 1.
        let m = mad(&[1.0, 1.0, 2.0, 2.0, 4.0, 6.0, 9.0]);
        assert!((m - 1.0).abs() < 1e-9);
    }

    #[test]
    fn linregress_matches_scipy() {
        let r = linregress_index(&[1.0, 2.0, 3.0, 4.0]);
        assert!((r.slope - 1.0).abs() < 1e-9);
        assert!((r.r_squared - 1.0).abs() < 1e-9);
        // Perfect fit → p-value is 0 in scipy.
        assert!(
            r.p_value < 1e-9,
            "perfect-fit p should be ~0, got {}",
            r.p_value
        );
    }

    #[test]
    fn student_t_matches_closed_forms() {
        // df=1 is Cauchy: p(t, 1) = 1 - (2/π)·atan(|t|).
        let cauchy_expected = |t: f64| 1.0 - (2.0 / std::f64::consts::PI) * t.abs().atan();
        for &t in &[0.25_f64, 0.75, 1.0, 2.0, 4.5] {
            let got = student_t_two_sided_p(t, 1.0);
            let want = cauchy_expected(t);
            assert!(
                (got - want).abs() < 1e-12,
                "df=1, t={t}: got {got}, want {want}"
            );
        }

        // df=2 has a closed form: p(t, 2) = 1 - |t|/sqrt(t²+2).
        let df2_expected = |t: f64| 1.0 - t.abs() / (t * t + 2.0).sqrt();
        for &t in &[0.1_f64, 0.5, 1.0, 2.0, 5.0] {
            let got = student_t_two_sided_p(t, 2.0);
            let want = df2_expected(t);
            assert!(
                (got - want).abs() < 1e-12,
                "df=2, t={t}: got {got}, want {want}"
            );
        }

        // Symmetry: p(-t) == p(t) for every df.
        for &df in &[2.0_f64, 3.0, 5.0, 10.0] {
            let p1 = student_t_two_sided_p(1.7, df);
            let p2 = student_t_two_sided_p(-1.7, df);
            assert!((p1 - p2).abs() < 1e-12);
        }
    }

    #[test]
    fn linregress_growing_trajectory_fires_on_three_points() {
        // Three monotonically increasing scores — scipy returns p ≈ 0.0659,
        // which is below the 0.15 threshold trajectory.rs uses.
        let r = linregress_index(&[0.30, 0.55, 0.80]);
        assert!(r.slope > 0.0);
        assert!(
            r.p_value < 0.15,
            "expected p<0.15 for clear upward trend, got {}",
            r.p_value
        );
    }

    #[test]
    fn linregress_two_points_reports_perfect_fit_as_growing() {
        // scipy.stats.linregress on any 2 non-equal points returns r=1
        // (perfect fit) and pvalue=0 — so the Python trajectory classifier
        // fires "growing"/"declining" on 2-sprint runs. Parity with that
        // behaviour is what lets `student_trajectory` checksum-match.
        let r = linregress_index(&[0.0857142857142857, 0.433090196243564]);
        assert!(r.slope > 0.0);
        assert!((r.r_squared - 1.0).abs() < 1e-12);
        assert_eq!(r.p_value, 0.0);
    }

    #[test]
    fn round_half_even_matches_python() {
        // Banker's rounding at tie values.
        assert_eq!(round_half_even(0.5, 0), 0.0);
        assert_eq!(round_half_even(1.5, 0), 2.0);
        assert_eq!(round_half_even(2.5, 0), 2.0);
        assert_eq!(round_half_even(3.5, 0), 4.0);
        assert_eq!(round_half_even(-0.5, 0), 0.0);
        assert_eq!(round_half_even(-1.5, 0), -2.0);

        // The parity-diff case: `sum([1.0, 1.0, 0.9998, 0.9893, 0.7762, 0.716]) / 6`
        // stores as 0.913549999…; Python's round(x, 4) returns 0.9135.
        let vals = [1.0_f64, 1.0, 0.9998, 0.9893, 0.7762, 0.716];
        let avg: f64 = vals.iter().sum::<f64>() / vals.len() as f64;
        assert_eq!(round_half_even(avg, 4), 0.9135);
    }
}
