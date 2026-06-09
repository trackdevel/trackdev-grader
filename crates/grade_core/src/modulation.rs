//! Per-task keep-factor until Phase 3 formula evaluation replaces this.

/// `keep = 1 − (1 − floor_keep)·strength·m·l`
pub fn keep(m: f64, l: f64, strength: f64, floor_keep: f64) -> f64 {
    1.0 - (1.0 - floor_keep) * strength * m * l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_frontier_e_matches_worked_example() {
        assert!((keep(1.0, 1.0, 1.0, 0.20) - 0.2).abs() < 1e-9);
    }

    #[test]
    fn keep_cap_a_is_full_retention() {
        assert!((keep(0.0, 0.0, 1.0, 0.20) - 1.0).abs() < 1e-9);
    }
}
