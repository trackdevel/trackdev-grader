//! Read edited grading weights from the Excel `Weights` sheet via calamine.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use calamine::{open_workbook, Data, Reader, Xlsx};

use crate::config::GradingConfig;
use crate::weights_layout::{
    K_CRIT_ROW, LABEL_COL, LEVEL_TABLE_START, MODEL_TABLE_START, SCALAR_START_ROW, VALUE_COL,
};

pub const WEIGHTS_SHEET_NAME: &str = "Weights";

/// Parse `Weights` from a grading workbook edited by the instructor.
pub fn import_weights(xlsx: &Path) -> Result<GradingConfig> {
    let mut workbook: Xlsx<_> =
        open_workbook(xlsx).with_context(|| format!("open grading workbook {}", xlsx.display()))?;

    if !workbook
        .sheet_names()
        .iter()
        .any(|name| name == WEIGHTS_SHEET_NAME)
    {
        bail!(
            "workbook {} has no '{WEIGHTS_SHEET_NAME}' sheet",
            xlsx.display()
        );
    }

    let range = workbook
        .worksheet_range(WEIGHTS_SHEET_NAME)
        .with_context(|| format!("read '{WEIGHTS_SHEET_NAME}' sheet"))?;

    let mut cfg = GradingConfig::default();

    cfg.weights_project.documentation = scalar_at(&range, SCALAR_START_ROW)?;
    cfg.weights_project.code_quality = scalar_at(&range, SCALAR_START_ROW + 1)?;
    cfg.weights_project.survival = scalar_at(&range, SCALAR_START_ROW + 2)?;
    cfg.weights_project.architecture = scalar_at(&range, SCALAR_START_ROW + 3)?;

    cfg.ai_usage.strength = scalar_at(&range, SCALAR_START_ROW + 4)?;
    cfg.ai_usage.floor_keep = scalar_at(&range, SCALAR_START_ROW + 5)?;
    cfg.ai_usage.undeclared_model_m = scalar_at(&range, SCALAR_START_ROW + 6)?;
    cfg.ai_usage.undeclared_level_l = scalar_at(&range, SCALAR_START_ROW + 7)?;

    cfg.penalty.max_penalty_points = scalar_at(&range, SCALAR_START_ROW + 8)?;
    cfg.penalty.student_penalty_cap = scalar_at(&range, SCALAR_START_ROW + 9)?;
    cfg.penalty.crit_sa_points = scalar_at(&range, SCALAR_START_ROW + 10)?;
    cfg.penalty.crit_cx_points = scalar_at(&range, SCALAR_START_ROW + 11)?;
    cfg.penalty.crit_flag_points = scalar_at(&range, SCALAR_START_ROW + 12)?;
    cfg.penalty.security_extra = scalar_at(&range, SCALAR_START_ROW + 13)?;

    cfg.normalization.doc_max = scalar_at(&range, SCALAR_START_ROW + 14)?;
    cfg.normalization.mi_floor = scalar_at(&range, SCALAR_START_ROW + 15)?;
    cfg.normalization.mi_ceiling = scalar_at(&range, SCALAR_START_ROW + 16)?;
    cfg.normalization.cc_penalty = scalar_at(&range, SCALAR_START_ROW + 17)?;
    cfg.normalization.test_bonus = scalar_at(&range, SCALAR_START_ROW + 18)?;
    cfg.normalization.test_cap = scalar_at(&range, SCALAR_START_ROW + 19)?;
    cfg.normalization.surv_floor = scalar_at(&range, SCALAR_START_ROW + 20)?;
    cfg.normalization.surv_ceiling = scalar_at(&range, SCALAR_START_ROW + 21)?;

    cfg.normalization.k_crit = scalar_at(&range, K_CRIT_ROW)?;
    cfg.normalization.k_warn = scalar_at(&range, K_CRIT_ROW + 1)?;
    cfg.normalization.arch_norm = scalar_at(&range, K_CRIT_ROW + 2)?;

    cfg.ai_usage.models = read_model_table(&range)?;
    cfg.ai_usage.levels = read_level_table(&range)?;

    Ok(cfg)
}

fn scalar_at(range: &calamine::Range<Data>, row: u32) -> Result<f64> {
    cell_f64(range, row, VALUE_COL)
        .with_context(|| format!("missing numeric weight at row {}", row + 1))
}

fn read_model_table(range: &calamine::Range<Data>) -> Result<BTreeMap<String, f64>> {
    let mut out = BTreeMap::new();
    let mut row = MODEL_TABLE_START;
    loop {
        if row >= LEVEL_TABLE_START {
            break;
        }
        let Some(name) = cell_string(range, row, LABEL_COL) else {
            break;
        };
        if name.is_empty() || name.starts_with("Nivell IA") {
            break;
        }
        let m = cell_f64(range, row, VALUE_COL)
            .with_context(|| format!("model '{name}' missing m value"))?;
        out.insert(name, m);
        row += 1;
    }
    if out.is_empty() {
        bail!("Weights sheet has no model→m table starting at row {}", MODEL_TABLE_START + 1);
    }
    Ok(out)
}

fn read_level_table(range: &calamine::Range<Data>) -> Result<BTreeMap<String, f64>> {
    let mut out = BTreeMap::new();
    let mut row = LEVEL_TABLE_START;
    loop {
        let Some(level) = cell_string(range, row, LABEL_COL) else {
            break;
        };
        if level.is_empty() {
            break;
        }
        let l = cell_f64(range, row, VALUE_COL)
            .with_context(|| format!("level '{level}' missing l value"))?;
        out.insert(level, l);
        row += 1;
    }
    if out.is_empty() {
        bail!(
            "Weights sheet has no level→l table starting at row {}",
            LEVEL_TABLE_START + 1
        );
    }
    Ok(out)
}

fn cell_f64(range: &calamine::Range<Data>, row: u32, col: u32) -> Option<f64> {
    match range.get_value((row, col)) {
        Some(Data::Float(v)) => Some(*v),
        Some(Data::Int(v)) => Some(*v as f64),
        Some(Data::String(s)) => s.parse().ok(),
        _ => None,
    }
}

fn cell_string(range: &calamine::Range<Data>, row: u32, col: u32) -> Option<String> {
    match range.get_value((row, col)) {
        Some(Data::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Data::Float(v)) => Some(v.to_string()),
        Some(Data::Int(v)) => Some(v.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{write_workbook_buffer, GradingConfig, WorkbookData};
    use sprint_grader_core::Database;
    use tempfile::tempdir;

    #[test]
    fn missing_workbook_errors_cleanly() {
        let err = import_weights(Path::new("/nonexistent/grading_sheet.xlsx")).unwrap_err();
        assert!(err.to_string().contains("open grading workbook"), "{err}");
    }

    #[test]
    fn round_trip_weights_through_xlsx() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("grading.db");
        let db = Database::open(&path).unwrap();
        db.create_tables().unwrap();
        db.conn
            .execute(
                "INSERT INTO projects (id, slug, name) VALUES (1, 't', 'T')",
                [],
            )
            .unwrap();

        let cfg = GradingConfig::default();
        let data = WorkbookData {
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            weights_version: cfg.weights_version(),
            results: vec![],
            project_axes: vec![],
            tasks: vec![],
            crit_flags: vec![],
            flag_rows: vec![],
            ai_detect_rows: vec![],
            llm_flag_rows: vec![],
            labels: crate::labels::WorkbookLabels::load(&db.conn).unwrap(),
        };
        let xlsx_path = dir.path().join("sheet.xlsx");
        let buf = write_workbook_buffer(&data, &cfg).unwrap();
        std::fs::write(&xlsx_path, buf).unwrap();

        let imported = import_weights(&xlsx_path).unwrap();
        assert!((imported.weights_project.documentation - 0.25).abs() < 1e-9);
        assert!((imported.ai_usage.strength - 1.0).abs() < 1e-9);
        assert!(imported.ai_usage.models.contains_key("Cap"));
        assert!((imported.ai_usage.levels["C"] - 0.5).abs() < 1e-9);
    }
}
