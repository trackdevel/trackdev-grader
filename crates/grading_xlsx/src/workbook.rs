//! Self-recalculating grading workbook (`rust_xlsxwriter` 0.94).

use std::path::Path;

use anyhow::{Context, Result};
use rust_xlsxwriter::{
    DataValidation, DataValidationRule, Format, Formula, Workbook, Worksheet, XlsxError,
};

use crate::config::GradingConfig;
use crate::data::{ProjectAxisRaw, WorkbookData};
use crate::excel_text::truncate_excel_cell;
use crate::import_weights::WEIGHTS_SHEET_NAME;
use crate::weights_layout::{K_CRIT_ROW, LEVEL_TABLE_START, MODEL_TABLE_START};

/// Defined names written to every grading workbook (Wave 4 acceptance).
pub const DEFINED_NAMES: &[&str] = &[
    "w_doc",
    "w_cq",
    "w_surv",
    "w_arch",
    "ai_strength",
    "floor_keep",
    "undeclared_model_m",
    "undeclared_level_l",
    "max_penalty_points",
    "student_penalty_cap",
    "crit_sa_points",
    "crit_cx_points",
    "crit_flag_points",
    "security_extra",
    "doc_max",
    "mi_floor",
    "mi_ceiling",
    "cc_penalty",
    "test_bonus",
    "test_cap",
    "surv_floor",
    "surv_ceiling",
    "k_crit",
    "k_warn",
    "arch_norm",
];

fn dec_format(decimals: u32) -> Format {
    let zeros = "0".repeat(decimals as usize);
    Format::new().set_num_format(format!("0.{zeros}"))
}

fn input_format() -> Format {
    Format::new().set_bold().set_unlocked()
}

fn header_format() -> Format {
    Format::new().set_bold()
}

fn fmt_num(v: f64, decimals: u32) -> String {
    format!("{:.prec$}", v, prec = decimals as usize)
}

fn xl_row_to_excel(row: u32) -> u32 {
    row + 1
}

fn write_cell(ws: &mut Worksheet, row: u32, col: u16, s: &str) -> Result<()> {
    ws.write_string(row, col, truncate_excel_cell(s).as_ref())
        .map(|_| ())
        .map_err(xlsx_err)
}

fn write_cell_fmt(ws: &mut Worksheet, row: u32, col: u16, s: &str, fmt: &Format) -> Result<()> {
    ws.write_string_with_format(row, col, truncate_excel_cell(s).as_ref(), fmt)
        .map(|_| ())
        .map_err(xlsx_err)
}

pub fn write_workbook(data: &WorkbookData, cfg: &GradingConfig, out: &Path) -> Result<()> {
    let mut workbook = build_workbook(data, cfg)?;
    workbook
        .save(out)
        .with_context(|| format!("save grading workbook {}", out.display()))?;
    Ok(())
}

pub fn write_workbook_buffer(data: &WorkbookData, cfg: &GradingConfig) -> Result<Vec<u8>> {
    let mut workbook = build_workbook(data, cfg)?;
    workbook
        .save_to_buffer()
        .map_err(|e| anyhow::anyhow!("save grading workbook to buffer: {e}"))
}

fn build_workbook(data: &WorkbookData, cfg: &GradingConfig) -> Result<Workbook> {
    let mut workbook = Workbook::new();
    let decimals = cfg.output.decimals;
    let num_fmt = dec_format(decimals);

    let (model_last, level_last) = write_weights_sheet(&mut workbook, cfg)?;
    define_names(&mut workbook, model_last, level_last)?;

    write_crit_flags_sheet(&mut workbook, data)?;
    write_flags_sheet(&mut workbook, data, cfg)?;
    write_ai_detect_sheet(&mut workbook, data)?;
    write_axis_sheets(&mut workbook, data, cfg)?;
    write_ai_usage_sheet(&mut workbook, data, cfg, model_last, level_last)?;
    write_team_points_sheet(&mut workbook, data, &num_fmt)?;
    write_project_grades_sheet(&mut workbook, data, cfg, &num_fmt)?;
    write_student_grades_sheet(&mut workbook, data, cfg, &num_fmt)?;
    write_llm_flags_sheet(&mut workbook, data)?;
    write_methodology_sheet(&mut workbook, data)?;

    let _ = decimals;
    Ok(workbook)
}

fn write_weights_sheet(workbook: &mut Workbook, cfg: &GradingConfig) -> Result<(u32, u32)> {
    let ws = workbook
        .add_worksheet()
        .set_name(WEIGHTS_SHEET_NAME)
        .map_err(xlsx_err)?;

    let input = input_format();
    let dv_decimal =
        DataValidation::new().allow_decimal_number(DataValidationRule::Between(0.0, 10.0));

    write_cell_fmt(ws, 0, 0, "Grading weights & anchors", &header_format())?;

    let scalar_rows: [(&str, f64); 22] = [
        ("w_doc (documentation)", cfg.weights_project.documentation),
        ("w_cq (code_quality)", cfg.weights_project.code_quality),
        ("w_surv (survival)", cfg.weights_project.survival),
        ("w_arch (architecture)", cfg.weights_project.architecture),
        ("ai_strength", cfg.ai_usage.strength),
        ("floor_keep", cfg.ai_usage.floor_keep),
        ("undeclared_model_m", cfg.ai_usage.undeclared_model_m),
        ("undeclared_level_l", cfg.ai_usage.undeclared_level_l),
        ("max_penalty_points", cfg.penalty.max_penalty_points),
        ("student_penalty_cap", cfg.penalty.student_penalty_cap),
        ("crit_sa_points", cfg.penalty.crit_sa_points),
        ("crit_cx_points", cfg.penalty.crit_cx_points),
        ("crit_flag_points", cfg.penalty.crit_flag_points),
        ("security_extra", cfg.penalty.security_extra),
        ("doc_max", cfg.normalization.doc_max),
        ("mi_floor", cfg.normalization.mi_floor),
        ("mi_ceiling", cfg.normalization.mi_ceiling),
        ("cc_penalty", cfg.normalization.cc_penalty),
        ("test_bonus", cfg.normalization.test_bonus),
        ("test_cap", cfg.normalization.test_cap),
        ("surv_floor", cfg.normalization.surv_floor),
        ("surv_ceiling", cfg.normalization.surv_ceiling),
    ];

    for (i, (label, value)) in scalar_rows.iter().enumerate() {
        let row = 1 + i as u32;
        write_cell(ws, row, 0, *label)?;
        ws.write_number_with_format(row, 1, *value, &input)?;
        ws.add_data_validation(row, 1, row, 1, &dv_decimal)?;
    }

    let k_row = K_CRIT_ROW;
    write_cell(ws, k_row, 0, "k_crit")?;
    ws.write_number_with_format(k_row, 1, cfg.normalization.k_crit, &input)?;
    ws.add_data_validation(k_row, 1, k_row, 1, &dv_decimal)?;
    write_cell(ws, k_row + 1, 0, "k_warn")?;
    ws.write_number_with_format(k_row + 1, 1, cfg.normalization.k_warn, &input)?;
    ws.add_data_validation(k_row + 1, 1, k_row + 1, 1, &dv_decimal)?;
    write_cell(ws, k_row + 2, 0, "arch_norm")?;
    ws.write_number_with_format(k_row + 2, 1, cfg.normalization.arch_norm, &input)?;
    ws.add_data_validation(k_row + 2, 1, k_row + 2, 1, &dv_decimal)?;

    write_cell(ws, MODEL_TABLE_START - 1, 0, "Model IA → m")?;
    write_cell(ws, MODEL_TABLE_START - 1, 1, "m")?;
    let mut model_row = MODEL_TABLE_START;
    for (name, m) in &cfg.ai_usage.models {
        write_cell(ws, model_row, 0, name)?;
        ws.write_number_with_format(model_row, 1, *m, &input)?;
        ws.add_data_validation(model_row, 1, model_row, 1, &dv_decimal)?;
        model_row += 1;
    }
    let model_last = model_row.saturating_sub(1);

    write_cell(ws, LEVEL_TABLE_START - 1, 0, "Nivell IA → l")?;
    write_cell(ws, LEVEL_TABLE_START - 1, 1, "l")?;
    let mut level_row = LEVEL_TABLE_START;
    for (level, l) in &cfg.ai_usage.levels {
        write_cell(ws, level_row, 0, level)?;
        ws.write_number_with_format(level_row, 1, *l, &input)?;
        ws.add_data_validation(level_row, 1, level_row, 1, &dv_decimal)?;
        level_row += 1;
    }
    let level_last = level_row.saturating_sub(1);

    ws.set_column_width(0, 28.0)?;
    ws.set_column_width(1, 14.0)?;
    ws.protect();
    Ok((model_last, level_last))
}

fn define_names(workbook: &mut Workbook, model_last: u32, level_last: u32) -> Result<()> {
    let w = WEIGHTS_SHEET_NAME;
    let defs: [(&str, &str); 25] = [
        ("w_doc", &format!("='{w}'!$B$2")),
        ("w_cq", &format!("='{w}'!$B$3")),
        ("w_surv", &format!("='{w}'!$B$4")),
        ("w_arch", &format!("='{w}'!$B$5")),
        ("ai_strength", &format!("='{w}'!$B$6")),
        ("floor_keep", &format!("='{w}'!$B$7")),
        ("undeclared_model_m", &format!("='{w}'!$B$8")),
        ("undeclared_level_l", &format!("='{w}'!$B$9")),
        ("max_penalty_points", &format!("='{w}'!$B$10")),
        ("student_penalty_cap", &format!("='{w}'!$B$11")),
        ("crit_sa_points", &format!("='{w}'!$B$12")),
        ("crit_cx_points", &format!("='{w}'!$B$13")),
        ("crit_flag_points", &format!("='{w}'!$B$14")),
        ("security_extra", &format!("='{w}'!$B$15")),
        ("doc_max", &format!("='{w}'!$B$16")),
        ("mi_floor", &format!("='{w}'!$B$17")),
        ("mi_ceiling", &format!("='{w}'!$B$18")),
        ("cc_penalty", &format!("='{w}'!$B$19")),
        ("test_bonus", &format!("='{w}'!$B$20")),
        ("test_cap", &format!("='{w}'!$B$21")),
        ("surv_floor", &format!("='{w}'!$B$22")),
        ("surv_ceiling", &format!("='{w}'!$B$23")),
        ("k_crit", &format!("='{w}'!$B$24")),
        ("k_warn", &format!("='{w}'!$B$25")),
        ("arch_norm", &format!("='{w}'!$B$26")),
    ];
    for (name, formula) in defs {
        workbook.define_name(name, formula).map_err(xlsx_err)?;
    }
    let _ = (model_last, level_last);
    Ok(())
}

fn write_axis_sheets(
    workbook: &mut Workbook,
    data: &WorkbookData,
    cfg: &GradingConfig,
) -> Result<()> {
    write_one_axis_sheet(workbook, "Docs", data, cfg, write_docs_row)?;
    write_one_axis_sheet(workbook, "Quality", data, cfg, write_quality_row)?;
    write_one_axis_sheet(workbook, "Survival", data, cfg, write_survival_row)?;
    write_one_axis_sheet(workbook, "Architecture", data, cfg, write_arch_row)?;
    Ok(())
}

fn write_one_axis_sheet(
    workbook: &mut Workbook,
    sheet: &str,
    data: &WorkbookData,
    cfg: &GradingConfig,
    write_fn: fn(
        &mut Worksheet,
        u32,
        &crate::grade::GradingResult,
        &ProjectAxisRaw,
        &GradingConfig,
    ) -> Result<()>,
) -> Result<()> {
    let ws = workbook.add_worksheet().set_name(sheet).map_err(xlsx_err)?;
    write_axis_headers(ws, sheet)?;
    for (i, result) in data.results.iter().enumerate() {
        let row = 1 + i as u32;
        let axis = data
            .project_axes
            .get(i)
            .expect("project_axes aligned with results");
        write_fn(ws, row, result, axis, cfg)?;
    }
    ws.protect();
    Ok(())
}

fn write_axis_headers(ws: &mut Worksheet, sheet: &str) -> Result<()> {
    let hdr = header_format();
    let base = ["project", "raw", "present", "score_0_10"];
    match sheet {
        "Quality" => {
            for (i, h) in [
                "project",
                "mi_raw",
                "cc_pct",
                "mutation_score",
                "present",
                "score_0_10",
            ]
            .iter()
            .enumerate()
            {
                write_cell_fmt(ws, 0, i as u16, *h, &hdr)?;
            }
        }
        _ => {
            for (i, h) in base.iter().enumerate() {
                write_cell_fmt(ws, 0, i as u16, *h, &hdr)?;
            }
        }
    }
    Ok(())
}

fn write_docs_row(
    ws: &mut Worksheet,
    row: u32,
    result: &crate::grade::GradingResult,
    axis: &ProjectAxisRaw,
    cfg: &GradingConfig,
) -> Result<()> {
    let er = xl_row_to_excel(row);
    write_cell(ws, row, 0, &result.project.name)?;
    if let Some(raw) = axis.documentation_raw {
        ws.write_number(row, 1, raw)?;
    }
    ws.write_number(row, 2, if axis.documentation_present { 1.0 } else { 0.0 })?;
    let formula = format!("IF(C{er}<>0,MEDIAN(0,10*MEDIAN(0,B{er}/doc_max,1),10),\"\")");
    let cached = axis
        .documentation_score
        .map(|v| fmt_num(v, cfg.output.decimals))
        .unwrap_or_default();
    ws.write_formula_with_format(
        row,
        3,
        Formula::new(formula).set_result(cached),
        &dec_format(cfg.output.decimals),
    )?;
    Ok(())
}

fn write_quality_row(
    ws: &mut Worksheet,
    row: u32,
    result: &crate::grade::GradingResult,
    axis: &ProjectAxisRaw,
    cfg: &GradingConfig,
) -> Result<()> {
    let er = xl_row_to_excel(row);
    write_cell(ws, row, 0, &result.project.name)?;
    if let Some(raw) = axis.code_quality_raw {
        ws.write_number(row, 1, raw)?;
    }
    if let Some(cc) = axis.cc_pct {
        ws.write_number(row, 2, cc)?;
    }
    if let Some(ms) = axis.mutation_score {
        ws.write_number(row, 3, ms)?;
    }
    ws.write_number(row, 4, if axis.code_quality_present { 1.0 } else { 0.0 })?;
    let formula = format!(
        "IF(E{er}<>0,MEDIAN(0,10*MEDIAN(0,(B{er}-mi_floor)/(mi_ceiling-mi_floor),1)-cc_penalty*(C{er}/100)+MIN(test_cap,test_bonus*D{er}),10),\"\")"
    );
    let cached = axis
        .code_quality_score
        .map(|v| fmt_num(v, cfg.output.decimals))
        .unwrap_or_default();
    ws.write_formula_with_format(
        row,
        5,
        Formula::new(formula).set_result(cached),
        &dec_format(cfg.output.decimals),
    )?;
    Ok(())
}

fn write_survival_row(
    ws: &mut Worksheet,
    row: u32,
    result: &crate::grade::GradingResult,
    axis: &ProjectAxisRaw,
    cfg: &GradingConfig,
) -> Result<()> {
    let er = xl_row_to_excel(row);
    write_cell(ws, row, 0, &result.project.name)?;
    if let Some(raw) = axis.survival_raw {
        ws.write_number(row, 1, raw)?;
    }
    ws.write_number(row, 2, if axis.survival_present { 1.0 } else { 0.0 })?;
    let formula = format!(
        "IF(C{er}<>0,MEDIAN(0,10*MEDIAN(0,(B{er}-surv_floor)/(surv_ceiling-surv_floor),1),10),\"\")"
    );
    let cached = axis
        .survival_score
        .map(|v| fmt_num(v, cfg.output.decimals))
        .unwrap_or_default();
    ws.write_formula_with_format(
        row,
        3,
        Formula::new(formula).set_result(cached),
        &dec_format(cfg.output.decimals),
    )?;
    Ok(())
}

fn write_arch_row(
    ws: &mut Worksheet,
    row: u32,
    result: &crate::grade::GradingResult,
    axis: &ProjectAxisRaw,
    cfg: &GradingConfig,
) -> Result<()> {
    let er = xl_row_to_excel(row);
    write_cell(ws, row, 0, &result.project.name)?;
    if let Some(density) = axis.architecture_density {
        ws.write_number(row, 1, density)?;
    }
    ws.write_number(row, 2, if axis.architecture_present { 1.0 } else { 0.0 })?;
    let formula = format!("IF(C{er}<>0,MEDIAN(0,10-MIN(10,B{er}),10),\"\")");
    let cached = axis
        .architecture_score
        .map(|v| fmt_num(v, cfg.output.decimals))
        .unwrap_or_default();
    ws.write_formula_with_format(
        row,
        3,
        Formula::new(formula).set_result(cached),
        &dec_format(cfg.output.decimals),
    )?;
    Ok(())
}

fn write_ai_usage_sheet(
    workbook: &mut Workbook,
    data: &WorkbookData,
    cfg: &GradingConfig,
    model_last: u32,
    level_last: u32,
) -> Result<()> {
    let ws = workbook
        .add_worksheet()
        .set_name("AI_Usage")
        .map_err(xlsx_err)?;
    let hdr = header_format();
    for (i, h) in [
        "project",
        "task",
        "assignee",
        "model",
        "level",
        "m",
        "l",
        "keep",
        "raw_pt",
        "effective_pt",
    ]
    .iter()
    .enumerate()
    {
        write_cell_fmt(ws, 0, i as u16, *h, &hdr)?;
    }

    let w = WEIGHTS_SHEET_NAME;
    let model_range = format!("'{w}'!$A${MODEL_TABLE_START}:$A${model_last}");
    let model_vals = format!("'{w}'!$B${MODEL_TABLE_START}:$B${model_last}");
    let level_range = format!("'{w}'!$A${LEVEL_TABLE_START}:$A${level_last}");
    let level_vals = format!("'{w}'!$B${LEVEL_TABLE_START}:$B${level_last}");

    for (i, task) in data.tasks.iter().enumerate() {
        let row = 1 + i as u32;
        let er = xl_row_to_excel(row);
        write_cell(ws, row, 0, &data.labels.project(task.project_id))?;
        write_cell(ws, row, 1, &data.labels.task(task.task_id))?;
        write_cell(ws, row, 2, &data.labels.student(&task.assignee_id))?;
        if let Some(ref m) = task.model {
            write_cell(ws, row, 3, m)?;
        }
        if let Some(ref l) = task.level {
            write_cell(ws, row, 4, l)?;
        }

        let m_formula = if task.declared && task.model.is_some() {
            format!("IFERROR(INDEX({model_vals},MATCH(D{er},{model_range},0)),1)")
        } else {
            "undeclared_model_m".to_string()
        };
        let l_formula = if task.declared && task.level.is_some() {
            format!("IFERROR(INDEX({level_vals},MATCH(E{er},{level_range},0)),1)")
        } else {
            "undeclared_level_l".to_string()
        };
        ws.write_formula(row, 5, m_formula.as_str())?;
        ws.write_formula(row, 6, l_formula.as_str())?;
        let keep_formula = format!("1-(1-floor_keep)*ai_strength*F{er}*G{er}");
        ws.write_formula(row, 7, keep_formula.as_str())?;
        ws.write_number(row, 8, task.raw_points)?;
        let eff_formula = format!("I{er}*H{er}");
        ws.write_formula(row, 9, eff_formula.as_str())?;
    }
    let _ = cfg;
    ws.protect();
    Ok(())
}

fn write_team_points_sheet(
    workbook: &mut Workbook,
    data: &WorkbookData,
    num_fmt: &Format,
) -> Result<()> {
    let ws = workbook
        .add_worksheet()
        .set_name("TeamPoints")
        .map_err(xlsx_err)?;
    let hdr = header_format();
    for (i, h) in [
        "project",
        "team_size",
        "sum_raw",
        "sum_eff",
        "mean_raw",
        "ai_factor",
    ]
    .iter()
    .enumerate()
    {
        write_cell_fmt(ws, 0, i as u16, *h, &hdr)?;
    }
    for (i, result) in data.results.iter().enumerate() {
        let row = 1 + i as u32;
        let er = xl_row_to_excel(row);
        write_cell(ws, row, 0, &result.project.name)?;
        ws.write_number(row, 1, result.project.team_size as f64)?;
        let sum_raw = format!("SUMIFS(AI_Usage!$I:$I,AI_Usage!$A:$A,A{er})");
        let sum_eff = format!("SUMIFS(AI_Usage!$J:$J,AI_Usage!$A:$A,A{er})");
        ws.write_formula_with_format(row, 2, sum_raw.as_str(), num_fmt)?;
        ws.write_formula_with_format(row, 3, sum_eff.as_str(), num_fmt)?;
        let mean_raw = format!("IF(C{er}>0,C{er}/B{er},0)");
        ws.write_formula_with_format(
            row,
            4,
            Formula::new(mean_raw).set_result(fmt_num(
                if result.project.team_size > 0 {
                    result.students.iter().map(|s| s.raw_points).sum::<f64>()
                        / result.project.team_size as f64
                } else {
                    0.0
                },
                2,
            )),
            num_fmt,
        )?;
        let ai_factor = format!("IF(C{er}>0,D{er}/C{er},1)");
        ws.write_formula_with_format(
            row,
            5,
            Formula::new(ai_factor).set_result(fmt_num(result.project.ai_factor, 2)),
            num_fmt,
        )?;
    }
    ws.protect();
    Ok(())
}

fn write_project_grades_sheet(
    workbook: &mut Workbook,
    data: &WorkbookData,
    cfg: &GradingConfig,
    num_fmt: &Format,
) -> Result<()> {
    let ws = workbook
        .add_worksheet()
        .set_name("ProjectGrades")
        .map_err(xlsx_err)?;
    let hdr = header_format();
    for (i, h) in [
        "project",
        "doc",
        "code_quality",
        "survival",
        "architecture",
        "Q",
        "project_penalty",
        "Q_pen",
        "ai_factor",
        "final",
        "team_size",
        "review_gate",
    ]
    .iter()
    .enumerate()
    {
        write_cell_fmt(ws, 0, i as u16, *h, &hdr)?;
    }
    for (i, result) in data.results.iter().enumerate() {
        let row = 1 + i as u32;
        let er = xl_row_to_excel(row);
        write_cell(ws, row, 0, &result.project.name)?;
        for (col, sheet) in [
            (1, "Docs"),
            (2, "Quality"),
            (3, "Survival"),
            (4, "Architecture"),
        ] {
            let score_col = if sheet == "Quality" { "F" } else { "D" };
            let xref = format!("{sheet}!{score_col}{er}");
            ws.write_formula(row, col, xref.as_str())?;
        }
        let q_formula = format!(
            "IFERROR((IF(Docs!C{er}<>0,Docs!D{er}*w_doc,0)+IF(Quality!E{er}<>0,Quality!F{er}*w_cq,0)+IF(Survival!C{er}<>0,Survival!D{er}*w_surv,0)+IF(Architecture!C{er}<>0,Architecture!D{er}*w_arch,0))/(IF(Docs!C{er}<>0,w_doc,0)+IF(Quality!E{er}<>0,w_cq,0)+IF(Survival!C{er}<>0,w_surv,0)+IF(Architecture!C{er}<>0,w_arch,0)),0)"
        );
        ws.write_formula_with_format(
            row,
            5,
            Formula::new(q_formula)
                .set_result(fmt_num(result.project.quality_grade, cfg.output.decimals)),
            num_fmt,
        )?;
        let pen_formula =
            format!("MIN(max_penalty_points,SUMIFS(CritFlags!$G:$G,CritFlags!$A:$A,A{er}))");
        ws.write_formula_with_format(
            row,
            6,
            Formula::new(pen_formula)
                .set_result(fmt_num(result.project.project_penalty, cfg.output.decimals)),
            num_fmt,
        )?;
        let qpen_formula = format!("MEDIAN(0,F{er}-G{er},10)");
        ws.write_formula_with_format(
            row,
            7,
            Formula::new(qpen_formula).set_result(fmt_num(
                result.project.quality_penalized,
                cfg.output.decimals,
            )),
            num_fmt,
        )?;
        ws.write_formula(
            row,
            8,
            Formula::new(format!(
                "INDEX(TeamPoints!$F:$F,MATCH(A{er},TeamPoints!$A:$A,0))"
            ))
            .set_result(fmt_num(result.project.ai_factor, cfg.output.decimals)),
        )?;
        let final_formula = format!("H{er}*I{er}");
        ws.write_formula_with_format(
            row,
            9,
            Formula::new(final_formula)
                .set_result(fmt_num(result.project.final_grade, cfg.output.decimals)),
            num_fmt,
        )?;
        ws.write_number(row, 10, result.project.team_size as f64)?;
        if let Some(ref gate) = result.project.review_gate {
            write_cell(ws, row, 11, gate)?;
        }
    }
    ws.protect();
    Ok(())
}

fn write_student_grades_sheet(
    workbook: &mut Workbook,
    data: &WorkbookData,
    cfg: &GradingConfig,
    num_fmt: &Format,
) -> Result<()> {
    let ws = workbook
        .add_worksheet()
        .set_name("StudentGrades")
        .map_err(xlsx_err)?;
    let hdr = header_format();
    for (i, h) in [
        "student",
        "project",
        "raw_points",
        "effective_points",
        "ai_keep_factor",
        "contribution_ratio",
        "base",
        "student_penalty",
        "final",
        "review_gate",
    ]
    .iter()
    .enumerate()
    {
        write_cell_fmt(ws, 0, i as u16, *h, &hdr)?;
    }
    let mut row = 1u32;
    for result in &data.results {
        for student in &result.students {
            let er = xl_row_to_excel(row);
            write_cell(ws, row, 0, &student.full_name)?;
            write_cell(ws, row, 1, &data.labels.project(student.project_id))?;
            let raw_f =
                format!("SUMIFS(AI_Usage!$I:$I,AI_Usage!$A:$A,$B{er},AI_Usage!$C:$C,$A{er})");
            ws.write_formula_with_format(
                row,
                2,
                Formula::new(raw_f).set_result(fmt_num(student.raw_points, cfg.output.decimals)),
                num_fmt,
            )?;
            let eff_f =
                format!("SUMIFS(AI_Usage!$J:$J,AI_Usage!$A:$A,$B{er},AI_Usage!$C:$C,$A{er})");
            ws.write_formula_with_format(
                row,
                3,
                Formula::new(eff_f)
                    .set_result(fmt_num(student.effective_points, cfg.output.decimals)),
                num_fmt,
            )?;
            let keep_f = format!("IF(C{er}>0,D{er}/C{er},\"\")");
            let keep_cached = student
                .ai_keep_factor
                .map(|v| fmt_num(v, cfg.output.decimals))
                .unwrap_or_default();
            ws.write_formula_with_format(
                row,
                4,
                Formula::new(keep_f).set_result(keep_cached),
                num_fmt,
            )?;
            let contrib_f = format!(
                "IF(INDEX(TeamPoints!$D:$D,MATCH($B{er},TeamPoints!$A:$A,0))>0,D{er}/INDEX(TeamPoints!$D:$D,MATCH($B{er},TeamPoints!$A:$A,0)),\"\")"
            );
            let contrib_cached = student
                .contribution_ratio
                .map(|v| fmt_num(v, cfg.output.decimals))
                .unwrap_or_default();
            ws.write_formula_with_format(
                row,
                5,
                Formula::new(contrib_f).set_result(contrib_cached),
                num_fmt,
            )?;
            let base_f = format!(
                "IF(INDEX(TeamPoints!$E:$E,MATCH($B{er},TeamPoints!$A:$A,0))>0,INDEX(ProjectGrades!$H:$H,MATCH($B{er},ProjectGrades!$A:$A,0))*D{er}/INDEX(TeamPoints!$E:$E,MATCH($B{er},TeamPoints!$A:$A,0)),0)"
            );
            ws.write_formula_with_format(
                row,
                6,
                Formula::new(base_f).set_result(fmt_num(student.base_grade, cfg.output.decimals)),
                num_fmt,
            )?;
            let pen_f = format!(
                "MIN(student_penalty_cap,SUMIFS(Flags!$G:$G,Flags!$A:$A,$B{er},Flags!$B:$B,$A{er}))"
            );
            ws.write_formula_with_format(
                row,
                7,
                Formula::new(pen_f)
                    .set_result(fmt_num(student.student_penalty, cfg.output.decimals)),
                num_fmt,
            )?;
            let final_f = format!("MEDIAN(0,G{er}-H{er},10)");
            ws.write_formula_with_format(
                row,
                8,
                Formula::new(final_f).set_result(fmt_num(student.final_grade, cfg.output.decimals)),
                num_fmt,
            )?;
            if let Some(ref gate) = student.review_gate {
                write_cell(ws, row, 9, gate)?;
            }
            row += 1;
        }
    }
    ws.protect();
    Ok(())
}

fn write_crit_flags_sheet(workbook: &mut Workbook, data: &WorkbookData) -> Result<()> {
    let ws = workbook
        .add_worksheet()
        .set_name("CritFlags")
        .map_err(xlsx_err)?;
    let hdr = header_format();
    for (i, h) in [
        "project",
        "repo",
        "kind",
        "rule_id",
        "severity",
        "category",
        "penalty_contribution",
    ]
    .iter()
    .enumerate()
    {
        write_cell_fmt(ws, 0, i as u16, *h, &hdr)?;
    }
    for (i, f) in data.crit_flags.iter().enumerate() {
        let row = 1 + i as u32;
        write_cell(ws, row, 0, &data.labels.project(f.project_id))?;
        write_cell(ws, row, 1, &f.repo_full_name)?;
        write_cell(ws, row, 2, &f.kind)?;
        write_cell(ws, row, 3, &f.rule_id)?;
        write_cell(ws, row, 4, &f.severity)?;
        if let Some(ref c) = f.category {
            write_cell(ws, row, 5, c)?;
        }
        ws.write_number(row, 6, f.penalty_points)?;
    }
    ws.protect();
    Ok(())
}

fn write_flags_sheet(
    workbook: &mut Workbook,
    data: &WorkbookData,
    cfg: &GradingConfig,
) -> Result<()> {
    let ws = workbook
        .add_worksheet()
        .set_name("Flags")
        .map_err(xlsx_err)?;
    let hdr = header_format();
    for (i, h) in [
        "project",
        "student",
        "sprint",
        "flag_type",
        "severity",
        "details",
        "penalty_contribution",
    ]
    .iter()
    .enumerate()
    {
        write_cell_fmt(ws, 0, i as u16, *h, &hdr)?;
    }
    for (i, f) in data.flag_rows.iter().enumerate() {
        let row = 1 + i as u32;
        write_cell(ws, row, 0, &data.labels.project(f.project_id))?;
        write_cell(ws, row, 1, &data.labels.student(&f.student_id))?;
        write_cell(ws, row, 2, &data.labels.sprint(f.sprint_id))?;
        write_cell(ws, row, 3, &f.flag_type)?;
        write_cell(ws, row, 4, &f.severity)?;
        if let Some(ref d) = f.details {
            write_cell(ws, row, 5, d)?;
        }
        let er = xl_row_to_excel(row);
        let pen_formula = format!("IF(E{er}=\"CRITICAL\",crit_flag_points,0)");
        let cached = if f.severity == "CRITICAL" {
            fmt_num(cfg.penalty.crit_flag_points, cfg.output.decimals)
        } else {
            fmt_num(0.0, cfg.output.decimals)
        };
        ws.write_formula(row, 6, Formula::new(pen_formula).set_result(cached))?;
    }
    ws.protect();
    Ok(())
}

fn write_ai_detect_sheet(workbook: &mut Workbook, data: &WorkbookData) -> Result<()> {
    let ws = workbook
        .add_worksheet()
        .set_name("AI_Detect")
        .map_err(xlsx_err)?;
    let hdr = header_format();
    for (i, h) in ["project", "student", "sprint", "risk_level"]
        .iter()
        .enumerate()
    {
        write_cell_fmt(ws, 0, i as u16, *h, &hdr)?;
    }
    for (i, r) in data.ai_detect_rows.iter().enumerate() {
        let row = 1 + i as u32;
        write_cell(ws, row, 0, &data.labels.project(r.project_id))?;
        write_cell(ws, row, 1, &data.labels.student(&r.student_id))?;
        write_cell(ws, row, 2, &data.labels.sprint(r.sprint_id))?;
        if let Some(ref rl) = r.risk_level {
            write_cell(ws, row, 3, rl)?;
        }
    }
    ws.protect();
    Ok(())
}

/// Advisory LLM quality flags (Track B); never a grade input.
pub const LLM_FLAGS_SHEET_NAME: &str = "LLM_Flags";

fn write_llm_flags_sheet(workbook: &mut Workbook, data: &WorkbookData) -> Result<()> {
    let ws = workbook
        .add_worksheet()
        .set_name(LLM_FLAGS_SHEET_NAME)
        .map_err(xlsx_err)?;
    let hdr = header_format();
    for (i, h) in [
        "project",
        "student",
        "sprint",
        "scope",
        "target_ref",
        "category",
        "severity",
        "summary",
    ]
    .iter()
    .enumerate()
    {
        write_cell_fmt(ws, 0, i as u16, *h, &hdr)?;
    }
    for (i, row) in data.llm_flag_rows.iter().enumerate() {
        let r = 1 + i as u32;
        write_cell(ws, r, 0, &data.labels.project(row.project_id))?;
        if let Some(sid) = &row.student_id {
            write_cell(ws, r, 1, &data.labels.student(sid))?;
        }
        if let Some(sid) = row.sprint_id {
            write_cell(ws, r, 2, &data.labels.sprint(sid))?;
        }
        write_cell(ws, r, 3, &row.scope)?;
        if let Some(t) = &row.target_ref {
            write_cell(ws, r, 4, &data.labels.humanize_target_ref(t))?;
        }
        write_cell(ws, r, 5, &row.category)?;
        write_cell(ws, r, 6, &row.severity)?;
        write_cell(ws, r, 7, &row.summary)?;
    }
    ws.protect();
    Ok(())
}

fn write_methodology_sheet(workbook: &mut Workbook, data: &WorkbookData) -> Result<()> {
    let ws = workbook
        .add_worksheet()
        .set_name("Methodology")
        .map_err(xlsx_err)?;
    let lines = [
        "Grading model (grading-sheet)",
        "",
        "Quality is measured once per team (documentation, code quality, survival, architecture)",
        "and is comparable across projects. Student grades redistribute the team quality grade",
        "by each member's share of AI-discounted effective story points.",
        "",
        "Team AI factor A = sum(effective) / sum(raw) affects the reported project grade only.",
        "For individuals, A cancels: base = Q_pen * eff_u / mean_raw.",
        "",
        "LLM flags (LLM_Flags sheet) are advisory context only — never grade inputs.",
        "",
        &format!("generated_at: {}", data.generated_at),
        &format!("weights_version: {}", data.weights_version),
    ];
    for (i, line) in lines.iter().enumerate() {
        write_cell(ws, i as u32, 0, *line)?;
    }
    ws.protect();
    Ok(())
}

fn xlsx_err(e: XlsxError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}
