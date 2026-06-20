//! Student-facing per-project final-grade report (`GRADES.md`).
//!
//! One Markdown file per project, written next to the team's `REPORT.md`. It
//! gives students maximum transparency over their grade **without disclosing
//! any formula or internal weight**: it shows the headline quantities (final
//! grade, contribution, AI factor) and, for penalties, a plain-language account
//! of *what* each penalty is — never the point deduction or how it is combined
//! into the grade.
//!
//! Labels are Catalan (the only student-facing locale), hard-coded here rather
//! than routed through an i18n table — mirroring `grade_xlsx`.
//!
//! The writer is fed a `grade_core::ProjectGrades` plus the project's raw
//! `StudentFlag` list so the penalty narrative reproduces exactly the set of
//! flags the grade model charges (the same filter as
//! `grade_core::shape`: graded behavioural CRITICAL flags, excluding the
//! per-student code-quality hotspots which are reported under their own
//! heading).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use grade_core::{behavioural_flag_graded, is_codequality_hotspot, ProjectGrades, StudentFlag};

/// Fixed filename for the student-facing grade report (one per project, beside
/// `REPORT.md`).
pub const GRADES_FILENAME: &str = "GRADES.md";

// --- Catalan labels -------------------------------------------------------

const TITLE: &str = "Notes";
const INTRO: &str = "Aquest document resumeix la nota de l'equip i la de cada membre. \
Mostra els valors finals i explica què significa cada concepte, però **no** les \
fórmules de càlcul ni els pesos interns.";

const TEAM_HEADER: &str = "Equip";
const STUDENTS_HEADER: &str = "Estudiants";
const PENALTIES_HEADER: &str = "Incidències que penalitzen";
const NO_PENALTIES: &str = "Cap incidència registrada.";

const NOTA_PROJECTE: &str = "Nota del projecte";
const QUALITAT_EQUIP: &str = "Qualitat de l'equip";
const FACTOR_IA_EQUIP: &str = "Factor d'ús d'IA";

const NOTA_FINAL: &str = "Nota final";
const CONTRIBUCIO_BRUTA: &str = "Contribució (punts originals)";
const FACTOR_IA: &str = "Factor d'ús d'IA";
const CONTRIBUCIO_EFECTIVA: &str = "Contribució efectiva (punts)";

/// Glosses for the team block (no formulas, no weights).
const GLOSS_NOTA_PROJECTE: &str =
    "Nota global de l'equip un cop aplicat el descompte per ús d'IA. És la base que es \
reparteix entre els membres segons la seva contribució.";
const GLOSS_QUALITAT_EQUIP: &str =
    "Indicador de qualitat del projecte (codi, arquitectura i altres mètriques de l'equip), \
ABANS d'aplicar el descompte per ús d'IA.";
const GLOSS_FACTOR_IA_EQUIP: &str =
    "Proporció de punts que conserva l'equip després de descomptar l'ús d'IA declarat \
(1 = cap descompte). Com més ús d'IA, més baix és aquest factor.";

/// Glosses for the per-student headline quantities.
const GLOSS_NOTA_FINAL: &str =
    "Nota individual final, entre 0 i 10, després d'aplicar les penalitzacions.";
const GLOSS_CONTRIBUCIO_BRUTA: &str =
    "Punts de història de les tasques acabades, abans d'ajustar per l'ús d'IA.";
const GLOSS_FACTOR_IA: &str =
    "Fracció de la feina que es considera pròpia segons les declaracions d'ús d'IA \
(1 = cap descompte; buit si no hi ha punts).";
const GLOSS_CONTRIBUCIO_EFECTIVA: &str =
    "Punts de història després d'ajustar per l'ús d'IA declarat a les tasques.";

// --- Catalan label helpers ------------------------------------------------

/// Catalan label for a code-quality dimension key.
fn dimension_label(dimension: &str) -> &str {
    match dimension {
        "architecture" => "Conformitat amb l'arquitectura",
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

/// Plain-language Catalan description of a behavioural penalty flag — *what* it
/// is, never how it is scored. `None` for unknown keys, in which case the caller
/// falls back to a humanised form of the raw key.
fn behaviour_flag_description(flag_type: &str) -> Option<&'static str> {
    let text = match flag_type {
        "CARRYING_TEAM" => "Concentració desproporcionada de la feina de l'equip en una sola persona.",
        "LOW_CODE_HIGH_POINTS" => "Punts de història elevats amb poca aportació de codi visible.",
        "POINT_CODE_MISMATCH" => "Desajust entre els punts reclamats i el codi efectivament aportat.",
        "CRAMMING" => "Acumulació de feina concentrada just abans del tancament del sprint.",
        "MICRO_PRS" => "Predominança de pull requests molt petits o fragmentats.",
        "SINGLE_COMMIT_DUMP" => "Pull request lliurada amb un únic commit gegant, sense un historial de treball incremental.",
        "AUTHOR_MISMATCH" => "L'autoria dels commits no coincideix amb la persona assignada a la tasca.",
        "ORPHAN_PR" => "Pull request fusionada sense cap tasca associada.",
        "FOREIGN_MERGE" => "Una tasca s'ha tancat amb una pull request escrita per una altra persona.",
        "UNKNOWN_CONTRIBUTOR" => "Commits amb una identitat d'autor no reconeguda dins l'equip.",
        "LOW_SURVIVAL_RATE" => "Una part important del codi aportat no sobreviu (es reescriu o s'elimina) al llarg del projecte.",
        "RAW_NORMALIZED_DIVERGENCE" => "Codi que coincideix estructuralment amb codi previ tret de canvis superficials (noms, literals).",
        "COSMETIC_REWRITE_ACTOR" => "Reescriptura superficial de codi aliè (canvis cosmètics que en reassignen l'autoria).",
        "COSMETIC_REWRITE_VICTIM" => "Codi propi reescrit superficialment per una altra persona.",
        "CROSS_TEAM_SIMILARITY" => "Codi amb una similitud molt alta amb el d'un altre equip.",
        "BULK_RENAME_PR" => "Pull request dominada per reanomenaments massius que es normalitzen a no-res.",
        "COSMETIC_HEAVY_PR" => "Pull request amb una proporció elevada de canvis cosmètics respecte del codi nou.",
        "LOW_DOC_SCORE" => "Documentació de les pull requests de baixa qualitat.",
        "LOW_REVIEWS" => "Participació molt baixa en la revisió de codi de l'equip.",
        "GHOST_CONTRIBUTOR" => "Tasques assignades sense aportació de codi visible associada.",
        "HIDDEN_CONTRIBUTOR" => "Aportació de codi elevada sense tasques que la reflecteixin.",
        "PR_DOES_NOT_COMPILE" => "Pull request fusionada que no compila.",
        "APPROVED_BROKEN_PR" => "Aprovació d'una pull request que no compila.",
        "HIGH_COMPILE_FAILURE_RATE" => "Proporció alta de pull requests que no compilen.",
        "LAST_MINUTE_PR" => "Pull requests obertes a l'últim moment abans del tancament.",
        "ALL_PRS_LATE" => "La majoria de pull requests s'han fet de manera tardana respecte del sprint.",
        "REGULARITY_DECLINING" => "Regularitat de treball decreixent al llarg dels sprints.",
        "LOW_MUTATION_SCORE" => "Cobertura de proves dèbil (les proves no detecten prou errors introduïts).",
        _ => return None,
    };
    Some(text)
}

/// Humanise a raw flag key as a last resort (`FOO_BAR` → `Foo bar`).
fn humanize_key(flag_type: &str) -> String {
    let mut out = String::with_capacity(flag_type.len());
    for (i, word) in flag_type.split('_').enumerate() {
        if word.is_empty() {
            continue;
        }
        if i > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.extend(chars.flat_map(|c| c.to_lowercase()));
        }
    }
    out
}

// --- Pure formatting helpers (unit-tested) --------------------------------

/// Format a number with up to `decimals` places, trailing zeros trimmed
/// (`5.0` → `"5"`, `5.50` → `"5.5"`). Mirrors `grade_xlsx`'s `dec_format`.
fn fmt_num(v: f64, decimals: u32) -> String {
    let mut s = format!("{v:.*}", decimals as usize);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

/// Format an AI factor / keep ratio at a fixed 3 places (`0.800`).
fn fmt_ratio(v: f64) -> String {
    format!("{v:.3}")
}

fn name_for<'a>(names: &'a BTreeMap<String, String>, student_id: &'a str) -> &'a str {
    names
        .get(student_id)
        .map(String::as_str)
        .unwrap_or(student_id)
}

/// The distinct graded behavioural CRITICAL penalty flags charged to a student,
/// in first-seen order. Mirrors the partition in `grade_core::shape`:
/// code-quality hotspots are excluded (reported separately) and
/// policy-ungraded flags (e.g. `ZERO_TASKS`) never appear.
fn graded_behaviour_flags<'a>(flags: &'a [StudentFlag], student_id: &str) -> Vec<&'a str> {
    let mut seen: Vec<&str> = Vec::new();
    for f in flags {
        if f.student_id != student_id {
            continue;
        }
        if f.severity != "CRITICAL" {
            continue;
        }
        if is_codequality_hotspot(&f.flag_type) || !behavioural_flag_graded(&f.flag_type) {
            continue;
        }
        if !seen.contains(&f.flag_type.as_str()) {
            seen.push(f.flag_type.as_str());
        }
    }
    seen
}

// --- Renderer -------------------------------------------------------------

/// Render the full `GRADES.md` body for one project. Pure — no I/O.
pub fn render_grades_markdown(
    project_name: &str,
    names: &BTreeMap<String, String>,
    grades: &ProjectGrades,
    student_flags: &[StudentFlag],
    decimals: u32,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {TITLE} — {project_name}");
    let _ = writeln!(out);
    let _ = writeln!(out, "> {INTRO}");
    let _ = writeln!(out);

    // --- Team block -------------------------------------------------------
    let _ = writeln!(out, "## {TEAM_HEADER}");
    let _ = writeln!(out);
    let _ = writeln!(out, "| Concepte | Valor |");
    let _ = writeln!(out, "|---|---|");
    let _ = writeln!(
        out,
        "| {NOTA_PROJECTE} | {} |",
        fmt_num(grades.project_final, decimals)
    );
    let _ = writeln!(
        out,
        "| {QUALITAT_EQUIP} | {} |",
        fmt_num(grades.quality_grade, decimals)
    );
    let _ = writeln!(
        out,
        "| {FACTOR_IA_EQUIP} | {} |",
        fmt_ratio(grades.ai_factor)
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "- **{NOTA_PROJECTE}**: {GLOSS_NOTA_PROJECTE}");
    let _ = writeln!(out, "- **{QUALITAT_EQUIP}**: {GLOSS_QUALITAT_EQUIP}");
    let _ = writeln!(out, "- **{FACTOR_IA_EQUIP}**: {GLOSS_FACTOR_IA_EQUIP}");
    let _ = writeln!(out);

    // --- Students ---------------------------------------------------------
    let _ = writeln!(out, "## {STUDENTS_HEADER}");
    let _ = writeln!(out);

    for stu in &grades.students {
        let _ = writeln!(out, "### {}", name_for(names, &stu.student_id));
        let _ = writeln!(out);
        let _ = writeln!(out, "| Concepte | Valor |");
        let _ = writeln!(out, "|---|---|");
        let _ = writeln!(
            out,
            "| {NOTA_FINAL} | {} |",
            fmt_num(stu.student_final, decimals)
        );
        let _ = writeln!(
            out,
            "| {CONTRIBUCIO_BRUTA} | {} |",
            fmt_num(stu.raw_points, decimals)
        );
        let ai = match stu.ai_keep {
            Some(k) => fmt_ratio(k),
            None => "—".to_string(),
        };
        let _ = writeln!(out, "| {FACTOR_IA} | {ai} |");
        let _ = writeln!(
            out,
            "| {CONTRIBUCIO_EFECTIVA} | {} |",
            fmt_num(stu.effective_points, decimals)
        );
        let _ = writeln!(out);
        let _ = writeln!(out, "- **{NOTA_FINAL}**: {GLOSS_NOTA_FINAL}");
        let _ = writeln!(out, "- **{CONTRIBUCIO_BRUTA}**: {GLOSS_CONTRIBUCIO_BRUTA}");
        let _ = writeln!(out, "- **{FACTOR_IA}**: {GLOSS_FACTOR_IA}");
        let _ = writeln!(
            out,
            "- **{CONTRIBUCIO_EFECTIVA}**: {GLOSS_CONTRIBUCIO_EFECTIVA}"
        );
        let _ = writeln!(out);

        // Penalties: what they are, not the point deduction.
        let _ = writeln!(out, "**{PENALTIES_HEADER}**");
        let _ = writeln!(out);
        let behaviour = graded_behaviour_flags(student_flags, &stu.student_id);
        let has_cq = !stu.codequality_components.is_empty();
        if behaviour.is_empty() && !has_cq {
            let _ = writeln!(out, "{NO_PENALTIES}");
        } else {
            for ft in &behaviour {
                let desc = behaviour_flag_description(ft)
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        format!(
                            "{} (incidència de procés o treball en equip).",
                            humanize_key(ft)
                        )
                    });
                let _ = writeln!(out, "- {desc}");
            }
            for comp in &stu.codequality_components {
                // `tier` is only populated in some paths; omit the band when
                // absent rather than render an empty "()".
                if comp.tier.is_empty() {
                    let _ = writeln!(
                        out,
                        "- {}: incidència de qualitat del codi.",
                        dimension_label(&comp.dimension),
                    );
                } else {
                    let _ = writeln!(
                        out,
                        "- {} ({}): incidència de qualitat del codi.",
                        dimension_label(&comp.dimension),
                        band_label(&comp.tier),
                    );
                }
            }
        }
        let _ = writeln!(out);
    }

    out
}

/// Write `GRADES.md` for one project to `out_path`.
///
/// `names` maps `student_id → full_name`; a missing id falls back to the id.
/// `student_flags` is the project's raw flag list (sprint + artifact);
/// `decimals` is the spec's display precision for grades/points.
pub fn write_grades_markdown(
    out_path: &Path,
    project_name: &str,
    names: &BTreeMap<String, String>,
    grades: &ProjectGrades,
    student_flags: &[StudentFlag],
    decimals: u32,
) -> Result<()> {
    let body = render_grades_markdown(project_name, names, grades, student_flags, decimals);
    std::fs::write(out_path, body).with_context(|| format!("write {}", out_path.display()))?;
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
            ai_factor: 0.95,
            project_final: 7.0,
            team_quality_penalty: 0.0,
            team_size: students.len() as i64,
            axes: vec![],
            extra_tech: 0.0,
            extra_tech_components: vec![],
            students,
        }
    }

    fn flag(sid: &str, severity: &str, ft: &str) -> StudentFlag {
        StudentFlag {
            student_id: sid.into(),
            severity: severity.into(),
            source: "sprint".into(),
            flag_type: ft.into(),
            weighted: None,
        }
    }

    #[test]
    fn fmt_num_trims_trailing_zeros() {
        assert_eq!(fmt_num(5.0, 2), "5");
        assert_eq!(fmt_num(5.5, 2), "5.5");
        assert_eq!(fmt_num(5.25, 2), "5.25");
        assert_eq!(fmt_num(0.0, 2), "0");
    }

    #[test]
    fn fmt_ratio_is_three_places() {
        assert_eq!(fmt_ratio(0.8), "0.800");
        assert_eq!(fmt_ratio(1.0), "1.000");
    }

    #[test]
    fn humanize_key_titlecases_words() {
        assert_eq!(humanize_key("LAST_MINUTE_PR"), "Last Minute Pr");
        assert_eq!(humanize_key("ORPHAN_PR"), "Orphan Pr");
    }

    #[test]
    fn graded_behaviour_filters_hotspots_and_ungraded_and_dedups() {
        let flags = vec![
            flag("alice", "CRITICAL", "GHOST_CONTRIBUTOR"),
            flag("alice", "CRITICAL", "GHOST_CONTRIBUTOR"), // dup
            flag("alice", "CRITICAL", "ARCHITECTURE_HOTSPOT"), // code-quality, excluded
            flag("alice", "CRITICAL", "ZERO_TASKS"),        // ungraded by policy
            flag("alice", "WARNING", "MICRO_PRS"),          // not critical
            flag("bob", "CRITICAL", "ORPHAN_PR"),           // other student
        ];
        let got = graded_behaviour_flags(&flags, "alice");
        assert_eq!(got, vec!["GHOST_CONTRIBUTOR"]);
    }

    #[test]
    fn every_known_description_is_substantive() {
        for ft in [
            "CARRYING_TEAM",
            "GHOST_CONTRIBUTOR",
            "PR_DOES_NOT_COMPILE",
            "CROSS_TEAM_SIMILARITY",
        ] {
            let d = behaviour_flag_description(ft).expect("known");
            assert!(d.len() > 15, "{ft}: {d}");
        }
        assert!(behaviour_flag_description("NOPE").is_none());
    }

    #[test]
    fn renders_team_and_student_blocks_without_points_in_penalties() {
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
            StudentGrades {
                effective_points: 0.0,
                contribution: None,
                ai_keep: None,
                codequality_penalty: 0.0,
                codequality_components: vec![],
                student_final: 0.0,
                ..mk_student("bob", vec![])
            },
        ]);
        let flags = vec![flag("alice", "CRITICAL", "GHOST_CONTRIBUTOR")];

        let md = render_grades_markdown("Team 07", &names, &grades, &flags, 2);

        // Team block headline quantities.
        assert!(md.contains("# Notes — Team 07"));
        assert!(md.contains("| Nota del projecte | 7 |"));
        assert!(md.contains("| Qualitat de l'equip | 7.5 |"));
        assert!(md.contains("| Factor d'ús d'IA | 0.950 |"));

        // Student block.
        assert!(md.contains("### Alice Liddell"));
        assert!(md.contains("| Nota final | 5.5 |"));
        // Behavioural + code-quality penalties listed by description.
        assert!(md.contains("Tasques assignades sense aportació"));
        assert!(md.contains("Conformitat amb l'arquitectura (crític)"));

        // bob: no name in map (falls back to id), no penalties.
        assert!(md.contains("### bob"));
        assert!(md.contains(NO_PENALTIES));

        // Transparency invariant: no raw point deductions leak into penalties.
        assert!(!md.contains("-1"));
        assert!(!md.contains("punts descomptats"));
    }
}
