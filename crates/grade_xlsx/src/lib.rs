//! Student-facing per-project final-grade workbook (`.xlsx`).
//!
//! One file per project, two sheets:
//!   - **Notes**: a team header block (project, team quality grade, team size)
//!     above a one-row-per-student grid of the headline quantities.
//!   - **Qualitat del codi**: one row per (student, code-quality dimension) that
//!     contributed a penalty, tinted by band.
//!
//! No formulas are shown. Each sheet has a gloss row under the column headers
//! with short Catalan explanations (not the exact grading formulas). Labels are
//! Catalan (the only student-facing locale); they are hard-coded `const`s here
//! rather than routed through an i18n table.
//!
//! The writer is fed a `grade_core::ProjectGrades` so it is shared verbatim
//! between the CLI (`grade-xlsx`) and the desktop app's `export_grade_xlsx`
//! Tauri command — the desktop hands over its WASM-computed grades, the CLI its
//! `grade_cohort` output.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use grade_core::ProjectGrades;
use rust_xlsxwriter::{Color, Format, FormatAlign, FormatBorder, Workbook};

// --- Catalan labels -------------------------------------------------------

const SHEET_NOTES: &str = "Notes";
const SHEET_CQ: &str = "Qualitat del codi";

const PROJECTE: &str = "Projecte";
const QUALITAT_EQUIP: &str = "Qualitat de l'equip";
const FACTOR_IA_EQUIP: &str = "Factor d'ús d'IA de l'equip";
const PENAL_QUALITAT_EQUIP: &str = "Penalització de qualitat (equip)";
const NOTA_PROJECTE: &str = "Nota del projecte (amb IA)";
const MIDA_EQUIP: &str = "Mida de l'equip";

const ESTUDIANT: &str = "Estudiant";
const NOTA_FINAL: &str = "Nota final";
const NOTA_BASE: &str = "Nota base";
const CONTRIBUCIO: &str = "Contribució";
const PUNTS_ORIGINALS: &str = "Punts originals";
const PUNTS_EFECTIUS: &str = "Punts efectius";
const FACTOR_IA: &str = "Factor IA";
const PENAL_COMPORTAMENT: &str = "Penalització de comportament";
const PENAL_QUALITAT: &str = "Penalització de qualitat de codi";
const TASQUES_SENSE_IA: &str = "Tasques sense declarar IA";

const DIMENSIO: &str = "Dimensió";
const BANDA: &str = "Banda";
const PUNTS: &str = "Punts";

/// Short Catalan gloss for the team header block (column C).
const EXPL_QUALITAT_EQUIP: &str =
    "Indicador de qualitat del projecte (codi, arquitectura i altres mètriques de l'equip), ABANS d'aplicar el descompte per ús d'IA.";
const EXPL_FACTOR_IA_EQUIP: &str =
    "Proporció de punts que conserva l'equip després de descomptar l'ús d'IA (punts efectius ÷ punts estimats de tot l'equip). La nota del projecte es multiplica per aquest factor: com més ús d'IA, més baixa la nota del projecte.";
const EXPL_PENAL_QUALITAT_EQUIP: &str =
    "Descompte col·lectiu per incidències de qualitat del codi (arquitectura, complexitat, anàlisi estàtica). El 80% de cada incidència recau en qui la va escriure (columna «Penalització de qualitat de codi») i el 20% es comparteix amb tot l'equip i resta de la nota del projecte. Valor negatiu.";
const EXPL_NOTA_PROJECTE: &str =
    "Nota global de l'equip un cop aplicat el descompte per ús d'IA. És la nota que es reparteix entre els estudiants segons la seva contribució; per això pot ser més baixa que la qualitat de l'equip.";
const EXPL_MIDA_EQUIP: &str =
    "Nombre d'estudiants matriculats al projecte; s'utilitza per repartir la nota del projecte.";

/// One gloss per [`NOTES_HEADERS`] column, same order.
const NOTES_EXPLANATIONS: &[&str] = &[
    "Nom complet de l'estudiant.",
    "Nota individual final, entre 0 i 10, després d'aplicar les penalitzacions.",
    "Porció de la nota del projecte que correspon a aquest estudiant segons la seva participació a l'equip.",
    "Quina part de la feina efectiva de l'equip se li atribueix (0 = cap, 1 = tota).",
    "Punts de història de les tasques acabades, abans d'ajustar per l'ús d'IA.",
    "Punts de història després d'ajustar per l'ús d'IA declarat a les tasques.",
    "Fracció de la feina que es considera pròpia de l'estudiant segons les declaracions d'IA (buit si no té punts).",
    "Descompte per banderes crítiques de procés i treball en equip (p. ex. cap tasca acabada, contribució invisible, aprovar un PR que no compila). Cada bandera resta punts fins a un màxim; valor negatiu. Les incidències de codi van a la columna següent.",
    "Descompte total per problemes de qualitat del codi (detall a la fulla «Qualitat del codi»).",
    "Tasques acabades sense declarar l'ús d'IA. Només informatiu; no modifica la nota.",
];

const NOTES_HEADERS: &[&str] = &[
    ESTUDIANT,
    NOTA_FINAL,
    NOTA_BASE,
    CONTRIBUCIO,
    PUNTS_ORIGINALS,
    PUNTS_EFECTIUS,
    FACTOR_IA,
    PENAL_COMPORTAMENT,
    PENAL_QUALITAT,
    TASQUES_SENSE_IA,
];

/// One gloss per [`CQ_HEADERS`] column, same order.
const CQ_EXPLANATIONS: &[&str] = &[
    "Nom complet de l'estudiant.",
    "Àrea avaluada: arquitectura, complexitat del codi o anàlisi estàtica.",
    "Gravetat relativa respecte a la resta de la cohort (crític o avís).",
    "Punts descomptats per aquesta dimensió (valors negatius).",
];

const CQ_HEADERS: &[&str] = &[ESTUDIANT, DIMENSIO, BANDA, PUNTS];

/// Catalan label for a code-quality dimension key.
fn dimension_label(dimension: &str) -> &str {
    match dimension {
        "architecture" => "Conformitat arquitectura",
        "complexity" => "Complexitat del codi",
        "static_analysis" => "Anàlisi estàtica",
        other => other,
    }
}

/// Catalan label for a code-quality band.
fn band_label(band: &str) -> &str {
    match band {
        "critical" => "crític",
        "warning" => "avís",
        other => other,
    }
}

// --- Pure helpers (unit-tested) -------------------------------------------

/// A filesystem-safe, lowercase slug of a project name.
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Collision-free filename for a project's grade workbook.
pub fn grade_workbook_filename(project_name: &str) -> String {
    let s = slug(project_name);
    let s = if s.is_empty() {
        "project".to_string()
    } else {
        s
    };
    format!("notes_{s}.xlsx")
}

/// A stored positive penalty magnitude as the signed value to display (a
/// deduction). Normalises `-0.0` to `0.0`.
fn fmt_penalty(magnitude: f64) -> f64 {
    if magnitude == 0.0 {
        0.0
    } else {
        -magnitude
    }
}

// --- Formats --------------------------------------------------------------

fn header_format() -> Format {
    Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0xD9E2F3))
        .set_border(FormatBorder::Thin)
        .set_align(FormatAlign::Center)
        .set_text_wrap()
}

fn label_format() -> Format {
    Format::new().set_bold()
}

fn dec_format(decimals: u32) -> Format {
    // Up to `decimals` places, trailing zeros trimmed: 5.0 → "5", 5.5 → "5.5".
    let fmt = if decimals == 0 {
        "0".to_string()
    } else {
        format!("0.{}", "#".repeat(decimals as usize))
    };
    Format::new().set_num_format(fmt)
}

fn ratio_format() -> Format {
    Format::new().set_num_format("0.000")
}

fn int_format() -> Format {
    Format::new().set_num_format("0")
}

fn critical_fill() -> Format {
    Format::new().set_background_color(Color::RGB(0xFFC7CE))
}

fn warning_fill() -> Format {
    Format::new().set_background_color(Color::RGB(0xFFEB9C))
}

fn explanation_format() -> Format {
    Format::new()
        .set_italic()
        .set_text_wrap()
        .set_background_color(Color::RGB(0xF2F2F2))
        .set_align(FormatAlign::Left)
        .set_border(FormatBorder::Thin)
}

fn write_explanation_row(
    sheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    explanations: &[&str],
    fmt: &Format,
) -> Result<()> {
    for (col, text) in explanations.iter().enumerate() {
        sheet.write_string_with_format(row, col as u16, *text, fmt)?;
    }
    sheet.set_row_height(row, 48.0)?;
    Ok(())
}

// --- Writer ---------------------------------------------------------------

/// Write the two-sheet grade workbook for one project to `out_path`.
///
/// `names` maps `student_id → full_name`; a missing id falls back to the id.
/// `decimals` is the spec's display precision for grades/points.
pub fn write_grade_workbook(
    out_path: &Path,
    project_name: &str,
    names: &BTreeMap<String, String>,
    grades: &ProjectGrades,
    decimals: u32,
) -> Result<()> {
    let mut workbook = Workbook::new();
    write_notes_sheet(&mut workbook, project_name, names, grades, decimals)?;
    write_cq_sheet(&mut workbook, names, grades, decimals)?;
    workbook
        .save(out_path)
        .with_context(|| format!("write {}", out_path.display()))?;
    Ok(())
}

fn name_for<'a>(names: &'a BTreeMap<String, String>, student_id: &'a str) -> &'a str {
    names
        .get(student_id)
        .map(String::as_str)
        .unwrap_or(student_id)
}

fn write_notes_sheet(
    workbook: &mut Workbook,
    project_name: &str,
    names: &BTreeMap<String, String>,
    grades: &ProjectGrades,
    decimals: u32,
) -> Result<()> {
    let sheet = workbook.add_worksheet();
    sheet.set_name(SHEET_NOTES)?;

    let label = label_format();
    let dec = dec_format(decimals);
    let ratio = ratio_format();
    let ints = int_format();
    let header = header_format();
    let expl = explanation_format();

    // Team header block (shared values): quality (pre-AI) → AI factor →
    // project grade (post-AI, what gets split) → team size, so the AI haircut
    // is visible and the lower student grades are explained.
    sheet.write_string_with_format(0, 0, PROJECTE, &label)?;
    sheet.write_string(0, 1, project_name)?;
    sheet.write_string_with_format(1, 0, QUALITAT_EQUIP, &label)?;
    sheet.write_number_with_format(1, 1, grades.quality_grade, &dec)?;
    sheet.write_string_with_format(1, 2, EXPL_QUALITAT_EQUIP, &expl)?;
    sheet.write_string_with_format(2, 0, FACTOR_IA_EQUIP, &label)?;
    sheet.write_number_with_format(2, 1, grades.ai_factor, &ratio)?;
    sheet.write_string_with_format(2, 2, EXPL_FACTOR_IA_EQUIP, &expl)?;
    sheet.write_string_with_format(3, 0, PENAL_QUALITAT_EQUIP, &label)?;
    sheet.write_number_with_format(3, 1, fmt_penalty(grades.team_quality_penalty), &dec)?;
    sheet.write_string_with_format(3, 2, EXPL_PENAL_QUALITAT_EQUIP, &expl)?;
    sheet.write_string_with_format(4, 0, NOTA_PROJECTE, &label)?;
    sheet.write_number_with_format(4, 1, grades.project_final, &dec)?;
    sheet.write_string_with_format(4, 2, EXPL_NOTA_PROJECTE, &expl)?;
    sheet.write_string_with_format(5, 0, MIDA_EQUIP, &label)?;
    sheet.write_number_with_format(5, 1, grades.team_size as f64, &ints)?;
    sheet.write_string_with_format(5, 2, EXPL_MIDA_EQUIP, &expl)?;

    // Table header (row 7), gloss row (8), students (9+).
    const HEADER_ROW: u32 = 7;
    const EXPL_ROW: u32 = HEADER_ROW + 1;
    const DATA_ROW: u32 = EXPL_ROW + 1;
    for (col, title) in NOTES_HEADERS.iter().enumerate() {
        sheet.write_string_with_format(HEADER_ROW, col as u16, *title, &header)?;
    }
    write_explanation_row(sheet, EXPL_ROW, NOTES_EXPLANATIONS, &expl)?;

    for (i, stu) in grades.students.iter().enumerate() {
        let row = DATA_ROW + i as u32;
        sheet.write_string(row, 0, name_for(names, &stu.student_id))?;
        sheet.write_number_with_format(row, 1, stu.student_final, &dec)?;
        sheet.write_number_with_format(row, 2, stu.base_grade, &dec)?;
        match stu.contribution {
            Some(c) => sheet.write_number_with_format(row, 3, c, &ratio)?,
            None => sheet.write_string(row, 3, "")?,
        };
        sheet.write_number_with_format(row, 4, stu.raw_points, &dec)?;
        sheet.write_number_with_format(row, 5, stu.effective_points, &dec)?;
        match stu.ai_keep {
            Some(k) => sheet.write_number_with_format(row, 6, k, &ratio)?,
            None => sheet.write_string(row, 6, "")?,
        };
        sheet.write_number_with_format(row, 7, fmt_penalty(stu.student_penalty), &dec)?;
        sheet.write_number_with_format(row, 8, fmt_penalty(stu.codequality_penalty), &dec)?;
        sheet.write_number_with_format(row, 9, stu.ai_undeclared_count as f64, &ints)?;
    }

    sheet.set_column_width(0, 28.0)?;
    for col in 1..=9u16 {
        sheet.set_column_width(col, 14.0)?;
    }
    sheet.set_column_width(2, 52.0)?;
    sheet.set_column_width(7, 22.0)?;
    // Header-block gloss rows wrap; give them room.
    for r in 1..=5u32 {
        sheet.set_row_height(r, 30.0)?;
    }
    sheet.set_row_height(EXPL_ROW, 64.0)?;
    Ok(())
}

fn write_cq_sheet(
    workbook: &mut Workbook,
    names: &BTreeMap<String, String>,
    grades: &ProjectGrades,
    decimals: u32,
) -> Result<()> {
    let sheet = workbook.add_worksheet();
    sheet.set_name(SHEET_CQ)?;

    let header = header_format();
    let expl = explanation_format();
    let dec = dec_format(decimals);
    let crit = critical_fill();
    let warn = warning_fill();

    const HEADER_ROW: u32 = 0;
    const EXPL_ROW: u32 = 1;
    const DATA_ROW: u32 = 2;
    for (col, title) in CQ_HEADERS.iter().enumerate() {
        sheet.write_string_with_format(HEADER_ROW, col as u16, *title, &header)?;
    }
    write_explanation_row(sheet, EXPL_ROW, CQ_EXPLANATIONS, &expl)?;

    let mut row = DATA_ROW;
    for stu in &grades.students {
        for comp in &stu.codequality_components {
            let fill = match comp.tier.as_str() {
                "critical" => Some(&crit),
                "warning" => Some(&warn),
                _ => None,
            };
            match fill {
                Some(f) => {
                    sheet.write_string_with_format(row, 0, name_for(names, &stu.student_id), f)?;
                    sheet.write_string_with_format(row, 1, dimension_label(&comp.dimension), f)?;
                    sheet.write_string_with_format(row, 2, band_label(&comp.tier), f)?;
                    sheet.write_number_with_format(row, 3, fmt_penalty(comp.points), &dec)?;
                }
                None => {
                    sheet.write_string(row, 0, name_for(names, &stu.student_id))?;
                    sheet.write_string(row, 1, dimension_label(&comp.dimension))?;
                    sheet.write_string(row, 2, band_label(&comp.tier))?;
                    sheet.write_number_with_format(row, 3, fmt_penalty(comp.points), &dec)?;
                }
            }
            row += 1;
        }
    }

    sheet.set_column_width(0, 28.0)?;
    sheet.set_column_width(1, 24.0)?;
    sheet.set_column_width(2, 12.0)?;
    sheet.set_column_width(3, 10.0)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grade_core::{CodeQualityComponent, StudentGrades};

    fn mk_student(id: &str, cq: Vec<CodeQualityComponent>) -> StudentGrades {
        StudentGrades {
            student_id: id.into(),
            raw_points: 10.0,
            effective_points: 8.0,
            ai_keep: Some(0.8),
            contribution: Some(0.25),
            base_grade: 6.0,
            student_penalty: 0.5,
            codequality_penalty: cq.iter().map(|c| c.points).sum(),
            codequality_components: cq,
            ai_undeclared_count: 2,
            student_final: 5.5,
        }
    }

    fn mk_grades(students: Vec<StudentGrades>) -> ProjectGrades {
        ProjectGrades {
            project_id: 1,
            quality_grade: 7.5,
            quality_penalized: 7.5,
            project_penalty: 0.0,
            ai_factor: 1.0,
            project_final: 7.5,
            team_quality_penalty: 0.0,
            team_size: students.len() as i64,
            axes: vec![],
            extra_tech: 0.0,
            extra_tech_components: vec![],
            students,
        }
    }

    #[test]
    fn slug_is_lowercase_and_filesystem_safe() {
        assert_eq!(slug("Team 01"), "team-01");
        assert_eq!(slug("equip/Àlfa!"), "equip-lfa");
        assert_eq!(slug("--a--b--"), "a-b");
    }

    #[test]
    fn filename_uses_slug_and_falls_back() {
        assert_eq!(grade_workbook_filename("Team 01"), "notes_team-01.xlsx");
        assert_eq!(grade_workbook_filename("!!!"), "notes_project.xlsx");
    }

    #[test]
    fn penalty_is_negated_and_zero_normalised() {
        assert_eq!(fmt_penalty(0.0), 0.0);
        assert!(fmt_penalty(0.0).is_sign_positive());
        assert_eq!(fmt_penalty(0.5), -0.5);
    }

    #[test]
    fn behavior_penalty_explanation_is_substantive() {
        let idx = NOTES_HEADERS
            .iter()
            .position(|h| *h == PENAL_COMPORTAMENT)
            .expect("behavior column");
        let text = NOTES_EXPLANATIONS[idx];
        assert!(
            text.contains("banderes crítiques"),
            "should mention critical flags: {text}"
        );
        assert!(
            text.contains("codi"),
            "should distinguish from code-quality penalty: {text}"
        );
    }

    #[test]
    fn column_explanations_align_with_headers() {
        assert_eq!(NOTES_HEADERS.len(), NOTES_EXPLANATIONS.len());
        assert_eq!(CQ_HEADERS.len(), CQ_EXPLANATIONS.len());
        for text in NOTES_EXPLANATIONS.iter().chain(CQ_EXPLANATIONS.iter()) {
            assert!(
                text.len() > 10,
                "explanation should be a short sentence: {text:?}"
            );
        }
    }

    #[test]
    fn team_header_explains_ai_haircut() {
        // The team block must make the AI discount visible so a high quality
        // grade next to lower student grades is not misleading.
        assert!(EXPL_FACTOR_IA_EQUIP.contains("IA"));
        assert!(EXPL_FACTOR_IA_EQUIP.contains("punts efectius"));
        assert!(EXPL_NOTA_PROJECTE.contains("descompte"));
        assert!(EXPL_NOTA_PROJECTE.contains("reparteix"));
        // Quality gloss now flags that it precedes the AI discount.
        assert!(EXPL_QUALITAT_EQUIP.contains("ABANS"));
    }

    #[test]
    fn labels_translate_known_keys_and_passthrough() {
        assert_eq!(dimension_label("architecture"), "Conformitat arquitectura");
        assert_eq!(dimension_label("unknown"), "unknown");
        assert_eq!(band_label("warning"), "avís");
        assert_eq!(band_label("other"), "other");
    }

    #[test]
    fn writes_a_nonempty_two_sheet_workbook() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(grade_workbook_filename("Team 07"));
        let mut names = BTreeMap::new();
        names.insert("alice".to_string(), "Alice Liddell".to_string());

        let comp = CodeQualityComponent {
            dimension: "architecture".into(),
            blame: 12.0,
            blame_per_point: 1.5,
            tier: "critical".into(),
            points: 1.0,
        };
        let grades = mk_grades(vec![
            mk_student("alice", vec![comp]),
            // Zero-effort student with no name in the map: still rendered.
            StudentGrades {
                effective_points: 0.0,
                contribution: None,
                ai_keep: None,
                codequality_penalty: 0.0,
                codequality_components: vec![],
                ai_undeclared_count: 0,
                student_final: 0.0,
                ..mk_student("bob", vec![])
            },
        ]);

        write_grade_workbook(&path, "Team 07", &names, &grades, 2).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert!(meta.len() > 0, "workbook should be non-empty");
    }
}
