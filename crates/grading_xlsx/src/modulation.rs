//! Per-task AI keep-factor (`keep_t`) from declared model/level scalars.

use std::collections::BTreeMap;

use tracing::warn;

use crate::config::AiUsageConfig;

/// `keep_t = 1 − (1 − floor_keep)·strength·m·l`.
pub fn keep(m: f64, l: f64, strength: f64, floor_keep: f64) -> f64 {
    1.0 - (1.0 - floor_keep) * strength * m * l
}

/// Resolve model string → `m ∈ [0,1]`. Unmapped models warn and default to `1.0`.
pub fn resolve_model_scalar(model: &str, models: &BTreeMap<String, f64>) -> f64 {
    match models.get(model) {
        Some(&m) => m,
        None => {
            warn!(
                model = model,
                "unmapped AI model in grading config; treating as frontier (m=1.0)"
            );
            1.0
        }
    }
}

/// Resolve level string → `l ∈ [0,1]`. Unknown levels default to `1.0` (conservative).
pub fn resolve_level_scalar(level: &str, levels: &BTreeMap<String, f64>) -> f64 {
    levels.get(level).copied().unwrap_or_else(|| {
        warn!(
            level = level,
            "unmapped AI level in grading config; treating as E (l=1.0)"
        );
        1.0
    })
}

/// Compute keep for a declared task.
pub fn keep_for_declared(model: &str, level: &str, cfg: &AiUsageConfig) -> f64 {
    let m = resolve_model_scalar(model, &cfg.models);
    let l = resolve_level_scalar(level, &cfg.levels);
    keep(m, l, cfg.strength, cfg.floor_keep)
}

/// Compute keep for an undeclared task (assumed model/level from config).
pub fn keep_for_undeclared(cfg: &AiUsageConfig) -> f64 {
    keep(
        cfg.undeclared_model_m,
        cfg.undeclared_level_l,
        cfg.strength,
        cfg.floor_keep,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_frontier_e_matches_worked_example() {
        let k = keep(1.0, 1.0, 1.0, 0.20);
        assert!((k - 0.2).abs() < 1e-9);
    }

    #[test]
    fn keep_cap_a_is_full_retention() {
        let k = keep(0.0, 0.0, 1.0, 0.20);
        assert!((k - 1.0).abs() < 1e-9);
    }

    #[test]
    fn strength_zero_disables_modulation() {
        let k = keep(1.0, 1.0, 0.0, 0.20);
        assert!((k - 1.0).abs() < 1e-9);
    }
}
