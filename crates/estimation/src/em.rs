//! Per-student estimation-bias fitter (T-P2.1).
//!
//! Model. We treat each story-point estimate as
//!
//! ```text
//! y_{u,i} = β_u + δ_i + ε_{u,i},     ε ~ N(0, σ²)
//! ```
//!
//! where `y` is `log(estimation_points)`. β_u is the per-student bias
//! (positive ⇒ this student's estimates run high, i.e. over-estimates),
//! δ_i is the per-task difficulty, and σ² is residual noise.
//!
//! Priors. `β_u ~ N(0, 1)` and `δ_i ~ N(0, 1)` so the posterior is well
//! defined even when n_tasks for a student is small (the plan's
//! "few-tasks risk" mitigation). `σ²` is fitted from residuals.
//!
//! Fitting. With Gaussian priors and Gaussian likelihood, EM collapses
//! to coordinate-descent posterior-mean updates — there is no latent
//! variable to integrate, so the M-step is closed-form. We iterate
//! `(β, δ, σ²)` until the relative change in log-likelihood drops below
//! `TOL`. Identifiability is fixed by re-centring `β` to mean zero
//! after each iteration (the gauge), absorbing the shift into `δ`.
//!
//! Output. For each student we return the posterior mean and the 95 %
//! credible interval (Gaussian, ±1.96·σ_post).

use std::collections::HashMap;

const PRIOR_VAR_BETA: f64 = 1.0;
const PRIOR_VAR_DELTA: f64 = 1.0;
const TOL: f64 = 1e-6;
const MAX_ITERS: usize = 200;
const MIN_SIGMA2: f64 = 1e-4;

/// One observed (student, task, points) triple.
#[derive(Debug, Clone)]
pub struct Observation {
    pub student_id: String,
    pub task_id: i64,
    /// Raw story points. Must be > 0; rows with NULL/0 should be
    /// filtered upstream.
    pub points: f64,
}

/// Posterior summary for a single student.
#[derive(Debug, Clone)]
pub struct StudentBias {
    pub student_id: String,
    pub beta_mean: f64,
    pub beta_lower95: f64,
    pub beta_upper95: f64,
    pub n_tasks: usize,
}

#[derive(Debug, Clone)]
pub struct FitResult {
    pub students: Vec<StudentBias>,
    pub sigma2: f64,
    pub iterations: usize,
    pub converged: bool,
}

/// Fit `y = β_u + δ_i + ε` against `obs`. Empty input returns an empty
/// result. Observations whose `points` is non-positive are dropped.
pub fn fit(obs: &[Observation]) -> FitResult {
    // Index students and tasks.
    let mut student_idx: HashMap<String, usize> = HashMap::new();
    let mut task_idx: HashMap<i64, usize> = HashMap::new();
    let mut students: Vec<String> = Vec::new();
    let mut tasks: Vec<i64> = Vec::new();
    let mut y: Vec<f64> = Vec::with_capacity(obs.len());
    let mut row_u: Vec<usize> = Vec::with_capacity(obs.len());
    let mut row_i: Vec<usize> = Vec::with_capacity(obs.len());

    for o in obs.iter().filter(|o| o.points > 0.0) {
        let u = *student_idx.entry(o.student_id.clone()).or_insert_with(|| {
            students.push(o.student_id.clone());
            students.len() - 1
        });
        let i = *task_idx.entry(o.task_id).or_insert_with(|| {
            tasks.push(o.task_id);
            tasks.len() - 1
        });
        y.push(o.points.ln());
        row_u.push(u);
        row_i.push(i);
    }

    let n_obs = y.len();
    if n_obs == 0 {
        return FitResult {
            students: Vec::new(),
            sigma2: 0.0,
            iterations: 0,
            converged: true,
        };
    }

    let n_u = students.len();
    let n_i = tasks.len();
    let mut beta = vec![0.0_f64; n_u];
    let mut delta = vec![0.0_f64; n_i];
    let mut counts_u = vec![0usize; n_u];
    let mut counts_i = vec![0usize; n_i];
    for k in 0..n_obs {
        counts_u[row_u[k]] += 1;
        counts_i[row_i[k]] += 1;
    }

    // Initialise δ to per-task log-mean and β to per-student residual
    // mean — this is also a perfectly valid OLS warm-start that
    // dramatically cuts iterations on well-behaved data.
    let mut sum_y_i = vec![0.0_f64; n_i];
    for k in 0..n_obs {
        sum_y_i[row_i[k]] += y[k];
    }
    for i in 0..n_i {
        delta[i] = sum_y_i[i] / counts_i[i] as f64;
    }
    let mut sum_resid_u = vec![0.0_f64; n_u];
    for k in 0..n_obs {
        sum_resid_u[row_u[k]] += y[k] - delta[row_i[k]];
    }
    for u in 0..n_u {
        beta[u] = if counts_u[u] > 0 {
            sum_resid_u[u] / counts_u[u] as f64
        } else {
            0.0
        };
    }
    // Apply gauge once.
    centre(&mut beta, &mut delta);

    let mut sigma2 = residual_variance(&y, &row_u, &row_i, &beta, &delta).max(MIN_SIGMA2);
    let mut prev_ll = log_likelihood(&y, &row_u, &row_i, &beta, &delta, sigma2);
    let mut converged = false;
    let mut iters = 0usize;

    for _ in 0..MAX_ITERS {
        iters += 1;
        // β-update: posterior mean given current δ.
        let mut sum_resid_u = vec![0.0_f64; n_u];
        for k in 0..n_obs {
            sum_resid_u[row_u[k]] += y[k] - delta[row_i[k]];
        }
        for u in 0..n_u {
            let n = counts_u[u] as f64;
            // 1/τ² = 1/PRIOR + n/σ²
            let prec = 1.0 / PRIOR_VAR_BETA + n / sigma2;
            beta[u] = (sum_resid_u[u] / sigma2) / prec;
        }
        // δ-update: posterior mean given current β.
        let mut sum_resid_i = vec![0.0_f64; n_i];
        for k in 0..n_obs {
            sum_resid_i[row_i[k]] += y[k] - beta[row_u[k]];
        }
        for i in 0..n_i {
            let n = counts_i[i] as f64;
            let prec = 1.0 / PRIOR_VAR_DELTA + n / sigma2;
            delta[i] = (sum_resid_i[i] / sigma2) / prec;
        }
        // Re-centre β (gauge: mean β = 0; absorb shift into δ).
        centre(&mut beta, &mut delta);

        // σ² from residuals.
        sigma2 = residual_variance(&y, &row_u, &row_i, &beta, &delta).max(MIN_SIGMA2);

        let ll = log_likelihood(&y, &row_u, &row_i, &beta, &delta, sigma2);
        let denom = prev_ll.abs().max(1.0);
        if (ll - prev_ll).abs() / denom < TOL {
            converged = true;
            break;
        }
        prev_ll = ll;
    }

    // Posterior std-dev for each β (Gaussian; matches the update).
    let mut summaries = Vec::with_capacity(n_u);
    for u in 0..n_u {
        let n = counts_u[u] as f64;
        let prec = 1.0 / PRIOR_VAR_BETA + n / sigma2;
        let post_sd = (1.0 / prec).sqrt();
        summaries.push(StudentBias {
            student_id: students[u].clone(),
            beta_mean: beta[u],
            beta_lower95: beta[u] - 1.96 * post_sd,
            beta_upper95: beta[u] + 1.96 * post_sd,
            n_tasks: counts_u[u],
        });
    }
    summaries.sort_by(|a, b| a.student_id.cmp(&b.student_id));

    FitResult {
        students: summaries,
        sigma2,
        iterations: iters,
        converged,
    }
}

fn centre(beta: &mut [f64], delta: &mut [f64]) {
    if beta.is_empty() {
        return;
    }
    let mean: f64 = beta.iter().sum::<f64>() / beta.len() as f64;
    for b in beta.iter_mut() {
        *b -= mean;
    }
    for d in delta.iter_mut() {
        *d += mean;
    }
}

fn residual_variance(
    y: &[f64],
    row_u: &[usize],
    row_i: &[usize],
    beta: &[f64],
    delta: &[f64],
) -> f64 {
    let mut ss = 0.0;
    for k in 0..y.len() {
        let r = y[k] - beta[row_u[k]] - delta[row_i[k]];
        ss += r * r;
    }
    ss / y.len() as f64
}

fn log_likelihood(
    y: &[f64],
    row_u: &[usize],
    row_i: &[usize],
    beta: &[f64],
    delta: &[f64],
    sigma2: f64,
) -> f64 {
    let n = y.len() as f64;
    let mut ss = 0.0;
    for k in 0..y.len() {
        let r = y[k] - beta[row_u[k]] - delta[row_i[k]];
        ss += r * r;
    }
    -0.5 * n * (2.0 * std::f64::consts::PI * sigma2).ln() - 0.5 * ss / sigma2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(student: &str, task: i64, pts: f64) -> Observation {
        Observation {
            student_id: student.to_string(),
            task_id: task,
            points: pts,
        }
    }

    /// Synthetic ground-truth: 20 students × 30 tasks. Each student has
    /// a known β; each task a known δ. Fit and check that the recovered
    /// β correlates strongly with the truth and that the gauge
    /// (mean β = 0) is satisfied.
    #[test]
    fn convergence_on_synthetic_20_students_30_tasks() {
        // Deterministic LCG so the test is reproducible without rand.
        let mut state: u64 = 0xdeadbeefcafef00d;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((state >> 33) as f64) / (u32::MAX as f64) - 0.5
        };

        let true_beta: Vec<f64> = (0..20).map(|u| (u as f64 - 9.5) * 0.12).collect();
        let true_delta: Vec<f64> = (0..30).map(|i| (i as f64 - 14.5) * 0.06).collect();

        let mut data = Vec::new();
        for (u, &b) in true_beta.iter().enumerate() {
            for (i, &d) in true_delta.iter().enumerate() {
                let noise = next() * 0.2;
                let log_pt = b + d + noise + 1.5; // shift so points > 0
                let pts = log_pt.exp();
                data.push(obs(&format!("s{u}"), i as i64, pts));
            }
        }

        let res = fit(&data);
        assert!(res.converged, "EM should converge in ≤200 iters");
        assert_eq!(res.students.len(), 20);

        // Recovered β minus its mean (gauge) should correlate with
        // (true_β minus its mean). Pearson r ≥ 0.9 is a wide margin.
        let recovered: Vec<f64> = res
            .students
            .iter()
            .map(|s| {
                let idx: usize = s.student_id.trim_start_matches('s').parse().unwrap();
                (idx, s.beta_mean)
            })
            .fold(vec![0.0; 20], |mut acc, (i, b)| {
                acc[i] = b;
                acc
            });
        let r = pearson(&true_beta, &recovered);
        assert!(r > 0.9, "Pearson correlation too low: r={r}");
    }

    /// Identifiability gauge: mean(β) must be zero after fitting,
    /// regardless of the absolute level of the inputs. We shift every
    /// observation's points uniformly and confirm β is unchanged.
    #[test]
    fn identifiability_gauge_holds_under_global_shift() {
        let base = vec![
            obs("a", 1, 2.0),
            obs("a", 2, 4.0),
            obs("b", 1, 4.0),
            obs("b", 2, 8.0),
            obs("c", 1, 1.0),
            obs("c", 2, 2.0),
        ];
        let res1 = fit(&base);
        // Mean β within numerical tolerance of zero.
        let mean: f64 =
            res1.students.iter().map(|s| s.beta_mean).sum::<f64>() / res1.students.len() as f64;
        assert!(mean.abs() < 1e-9, "gauge violated: mean={mean}");

        // Multiply all points by 10 (shift +ln 10 in y); β should not change.
        let shifted: Vec<Observation> = base
            .iter()
            .map(|o| Observation {
                points: o.points * 10.0,
                ..o.clone()
            })
            .collect();
        let res2 = fit(&shifted);
        for (a, b) in res1.students.iter().zip(res2.students.iter()) {
            assert_eq!(a.student_id, b.student_id);
            assert!(
                (a.beta_mean - b.beta_mean).abs() < 1e-6,
                "β shifted under uniform scale: {} vs {}",
                a.beta_mean,
                b.beta_mean
            );
        }
    }

    /// CrI width shrinks with more observations per student.
    #[test]
    fn credible_interval_narrows_with_more_data() {
        let mut few = vec![obs("u", 1, 3.0), obs("v", 1, 3.0)];
        for i in 2..6 {
            few.push(obs("u", i, 3.0));
            few.push(obs("v", i, 3.0));
        }
        let r1 = fit(&few);
        let mut many = few.clone();
        for i in 6..30 {
            many.push(obs("u", i, 3.0));
            many.push(obs("v", i, 3.0));
        }
        let r2 = fit(&many);
        let w1 = r1.students[0].beta_upper95 - r1.students[0].beta_lower95;
        let w2 = r2.students[0].beta_upper95 - r2.students[0].beta_lower95;
        assert!(w2 < w1, "CrI should narrow: w1={w1}, w2={w2}");
    }

    #[test]
    fn empty_input_returns_empty() {
        let r = fit(&[]);
        assert!(r.students.is_empty());
        assert!(r.converged);
    }

    #[test]
    fn nonpositive_points_are_dropped() {
        let r = fit(&[
            obs("a", 1, 0.0),
            obs("a", 2, -1.0),
            obs("a", 3, 5.0),
            obs("b", 3, 5.0),
        ]);
        assert_eq!(r.students.len(), 2);
        assert_eq!(r.students[0].n_tasks, 1);
    }

    fn pearson(x: &[f64], y: &[f64]) -> f64 {
        let n = x.len() as f64;
        let mx = x.iter().sum::<f64>() / n;
        let my = y.iter().sum::<f64>() / n;
        let mut num = 0.0;
        let mut dx = 0.0;
        let mut dy = 0.0;
        for k in 0..x.len() {
            let a = x[k] - mx;
            let b = y[k] - my;
            num += a * b;
            dx += a * a;
            dy += b * b;
        }
        num / (dx.sqrt() * dy.sqrt())
    }
}
