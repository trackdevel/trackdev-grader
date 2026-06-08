//! Build the embedded, denormalized SQLite snapshot from `WorkbookData` +
//! `GradingConfig`.
//!
//! This is *presentation output*, not a copy of `grading.db`: only the columns
//! the page and the JS engine need are materialized. The engine recomputes
//! `derived_project` / `derived_student` at runtime; the `reference_*` tables
//! here are the Rust-computed grades the parity self-test pins against.
//!
//! A `NamedTempFile`-backed `Connection` is used (not an in-memory DB) so the
//! bytes come from a real on-disk SQLite file — we don't rely on the bundled
//! rusqlite's in-memory serialization.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use sprint_grader_grading_xlsx::{GradingConfig, WorkbookData};

/// Serialize a fresh snapshot DB to bytes.
pub fn build_snapshot_bytes(data: &WorkbookData, cfg: &GradingConfig) -> Result<Vec<u8>> {
    let tmp = tempfile::NamedTempFile::new().context("create snapshot temp file")?;
    let path = tmp.path().to_path_buf();
    {
        let conn = Connection::open(&path).context("open snapshot db")?;
        create_schema(&conn)?;
        insert_meta(&conn, data, cfg)?;
        insert_weights(&conn, cfg)?;
        insert_models_levels(&conn, cfg)?;
        insert_projects(&conn, data)?;
        insert_project_axis(&conn, data)?;
        insert_students(&conn, data)?;
        insert_tasks(&conn, data)?;
        insert_crit_flags(&conn, data)?;
        insert_flags(&conn, data)?;
        insert_ai_detect(&conn, data)?;
        insert_llm_flags(&conn, data)?;
        insert_reference(&conn, data)?;
        insert_label_lookups(&conn, data)?;
        create_views(&conn)?;
    } // drop connection: flushes + removes the rollback journal, leaving one file.

    std::fs::read(&path).context("read snapshot bytes")
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE meta (
            generated_at TEXT, weights_version TEXT, decimals INTEGER,
            quantize_final REAL, penalty_mode TEXT
         );
         CREATE TABLE weights (name TEXT, value REAL);
         CREATE TABLE models (name TEXT, m REAL);
         CREATE TABLE levels (name TEXT, l REAL);
         CREATE TABLE project (project_id INTEGER, name TEXT, team_size INTEGER);
         CREATE TABLE project_axis (
            project_id INTEGER,
            documentation_raw REAL, documentation_present INTEGER, documentation_score REAL,
            code_quality_raw REAL, cc_pct REAL, mutation_score REAL,
            code_quality_present INTEGER, code_quality_score REAL,
            survival_raw REAL, survival_present INTEGER, survival_score REAL,
            architecture_density REAL, arch_crit_count INTEGER, arch_warn_count INTEGER,
            architecture_present INTEGER, architecture_score REAL
         );
         CREATE TABLE student (student_id TEXT, project_id INTEGER, full_name TEXT);
         CREATE TABLE task (
            project_id INTEGER, task_id INTEGER, assignee_id TEXT,
            raw_points REAL, ai_model TEXT, ai_level TEXT, declared INTEGER,
            captured_at TEXT
         );
         CREATE TABLE crit_flag (
            project_id INTEGER, repo_full_name TEXT, kind TEXT,
            rule_id TEXT, severity TEXT, category TEXT
         );
         CREATE TABLE flag (
            project_id INTEGER, student_id TEXT, sprint_id INTEGER,
            flag_type TEXT, severity TEXT, details TEXT, source TEXT
         );
         CREATE TABLE ai_detect (
            project_id INTEGER, student_id TEXT, sprint_id INTEGER, risk_level TEXT
         );
         CREATE TABLE llm_flag (
            project_id INTEGER, student_id TEXT, sprint_id INTEGER, scope TEXT,
            target_ref TEXT, category TEXT, severity TEXT, summary TEXT
         );
         CREATE TABLE reference_student (
            student_id TEXT, project_id INTEGER, final_grade REAL, base_grade REAL,
            ai_keep REAL, contribution REAL, stu_pen REAL, review_gate TEXT
         );
         CREATE TABLE reference_project (
            project_id INTEGER, quality_grade REAL, quality_penalized REAL,
            ai_factor REAL, final_grade REAL, review_gate TEXT
         );
         CREATE TABLE label_sprint (sprint_id INTEGER PRIMARY KEY, label TEXT);
         CREATE TABLE label_target (target_ref TEXT PRIMARY KEY, label TEXT);
         CREATE TABLE label_task (task_id INTEGER PRIMARY KEY, label TEXT);",
    )
    .context("create snapshot schema")?;
    Ok(())
}

fn insert_meta(conn: &Connection, data: &WorkbookData, cfg: &GradingConfig) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (generated_at, weights_version, decimals, quantize_final, penalty_mode)
         VALUES (?, ?, ?, ?, ?)",
        params![
            data.generated_at,
            data.weights_version,
            cfg.output.decimals,
            cfg.output.quantize_final,
            cfg.penalty.mode,
        ],
    )?;
    Ok(())
}

/// The 25 scalar knobs the parity contract pins, as name/value rows. Names
/// match `workbook::DEFINED_NAMES`; the four weights map
/// `documentation→w_doc`, `code_quality→w_cq`, `survival→w_surv`,
/// `architecture→w_arch`.
fn insert_weights(conn: &Connection, cfg: &GradingConfig) -> Result<()> {
    let w = &cfg.weights_project;
    let a = &cfg.ai_usage;
    let p = &cfg.penalty;
    let n = &cfg.normalization;
    let pairs: [(&str, f64); 25] = [
        ("w_doc", w.documentation),
        ("w_cq", w.code_quality),
        ("w_surv", w.survival),
        ("w_arch", w.architecture),
        ("ai_strength", a.strength),
        ("floor_keep", a.floor_keep),
        ("undeclared_model_m", a.undeclared_model_m),
        ("undeclared_level_l", a.undeclared_level_l),
        ("max_penalty_points", p.max_penalty_points),
        ("student_penalty_cap", p.student_penalty_cap),
        ("crit_sa_points", p.crit_sa_points),
        ("crit_cx_points", p.crit_cx_points),
        ("crit_flag_points", p.crit_flag_points),
        ("security_extra", p.security_extra),
        ("doc_max", n.doc_max),
        ("mi_floor", n.mi_floor),
        ("mi_ceiling", n.mi_ceiling),
        ("cc_penalty", n.cc_penalty),
        ("test_bonus", n.test_bonus),
        ("test_cap", n.test_cap),
        ("surv_floor", n.surv_floor),
        ("surv_ceiling", n.surv_ceiling),
        ("k_crit", n.k_crit),
        ("k_warn", n.k_warn),
        ("arch_norm", n.arch_norm),
    ];
    let mut stmt = conn.prepare("INSERT INTO weights (name, value) VALUES (?, ?)")?;
    for (name, value) in pairs {
        stmt.execute(params![name, value])?;
    }
    Ok(())
}

fn insert_models_levels(conn: &Connection, cfg: &GradingConfig) -> Result<()> {
    let mut ms = conn.prepare("INSERT INTO models (name, m) VALUES (?, ?)")?;
    for (name, m) in &cfg.ai_usage.models {
        ms.execute(params![name, m])?;
    }
    let mut ls = conn.prepare("INSERT INTO levels (name, l) VALUES (?, ?)")?;
    for (name, l) in &cfg.ai_usage.levels {
        ls.execute(params![name, l])?;
    }
    Ok(())
}

fn insert_projects(conn: &Connection, data: &WorkbookData) -> Result<()> {
    let mut stmt =
        conn.prepare("INSERT INTO project (project_id, name, team_size) VALUES (?, ?, ?)")?;
    for r in &data.results {
        stmt.execute(params![
            r.project.project_id,
            r.project.name,
            r.project.team_size
        ])?;
    }
    Ok(())
}

fn insert_project_axis(conn: &Connection, data: &WorkbookData) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO project_axis
         (project_id, documentation_raw, documentation_present, documentation_score,
          code_quality_raw, cc_pct, mutation_score, code_quality_present, code_quality_score,
          survival_raw, survival_present, survival_score,
          architecture_density, arch_crit_count, arch_warn_count, architecture_present,
          architecture_score)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    // `project_axes[i]` aligns 1:1 with `results[i]` (both built in the same
    // loop), so the project id comes from the parallel result row.
    for (r, ax) in data.results.iter().zip(data.project_axes.iter()) {
        stmt.execute(params![
            r.project.project_id,
            ax.documentation_raw,
            ax.documentation_present,
            ax.documentation_score,
            ax.code_quality_raw,
            ax.cc_pct,
            ax.mutation_score,
            ax.code_quality_present,
            ax.code_quality_score,
            ax.survival_raw,
            ax.survival_present,
            ax.survival_score,
            ax.architecture_density,
            ax.arch_crit_count,
            ax.arch_warn_count,
            ax.architecture_present,
            ax.architecture_score,
        ])?;
    }
    Ok(())
}

fn insert_students(conn: &Connection, data: &WorkbookData) -> Result<()> {
    let mut stmt =
        conn.prepare("INSERT INTO student (student_id, project_id, full_name) VALUES (?, ?, ?)")?;
    for r in &data.results {
        for s in &r.students {
            stmt.execute(params![s.student_id, s.project_id, s.full_name])?;
        }
    }
    Ok(())
}

fn insert_tasks(conn: &Connection, data: &WorkbookData) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO task (project_id, task_id, assignee_id, raw_points, ai_model, ai_level, declared, captured_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    for t in &data.tasks {
        stmt.execute(params![
            t.project_id,
            t.task_id,
            t.assignee_id,
            t.raw_points,
            t.model,
            t.level,
            t.declared,
            t.captured_at,
        ])?;
    }
    Ok(())
}

fn insert_crit_flags(conn: &Connection, data: &WorkbookData) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO crit_flag (project_id, repo_full_name, kind, rule_id, severity, category)
         VALUES (?, ?, ?, ?, ?, ?)",
    )?;
    for cf in &data.crit_flags {
        stmt.execute(params![
            cf.project_id,
            cf.repo_full_name,
            cf.kind,
            cf.rule_id,
            cf.severity,
            cf.category,
        ])?;
    }
    Ok(())
}

/// Union of sprint flags (`source='sprint'`) and artifact flags
/// (`source='artifact'`, sprint/details NULL). Each row carries `severity` so
/// the engine sums CRITICAL student penalties across both sources.
fn insert_flags(conn: &Connection, data: &WorkbookData) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO flag (project_id, student_id, sprint_id, flag_type, severity, details, source)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )?;
    for f in &data.flag_rows {
        stmt.execute(params![
            f.project_id,
            f.student_id,
            f.sprint_id,
            f.flag_type,
            f.severity,
            f.details,
            "sprint",
        ])?;
    }
    for af in &data.artifact_flag_rows {
        stmt.execute(params![
            af.project_id,
            af.student_id,
            Option::<i64>::None,
            af.flag_type,
            af.severity,
            Option::<String>::None,
            "artifact",
        ])?;
    }
    Ok(())
}

fn insert_ai_detect(conn: &Connection, data: &WorkbookData) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO ai_detect (project_id, student_id, sprint_id, risk_level)
         VALUES (?, ?, ?, ?)",
    )?;
    for a in &data.ai_detect_rows {
        stmt.execute(params![
            a.project_id,
            a.student_id,
            a.sprint_id,
            a.risk_level
        ])?;
    }
    Ok(())
}

fn insert_llm_flags(conn: &Connection, data: &WorkbookData) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO llm_flag
         (project_id, student_id, sprint_id, scope, target_ref, category, severity, summary)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    for l in &data.llm_flag_rows {
        stmt.execute(params![
            l.project_id,
            l.student_id,
            l.sprint_id,
            l.scope,
            l.target_ref,
            l.category,
            l.severity,
            l.summary,
        ])?;
    }
    Ok(())
}

fn insert_reference(conn: &Connection, data: &WorkbookData) -> Result<()> {
    let mut ps = conn.prepare(
        "INSERT INTO reference_project
         (project_id, quality_grade, quality_penalized, ai_factor, final_grade, review_gate)
         VALUES (?, ?, ?, ?, ?, ?)",
    )?;
    let mut ss = conn.prepare(
        "INSERT INTO reference_student
         (student_id, project_id, final_grade, base_grade, ai_keep, contribution, stu_pen,
          review_gate)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    for r in &data.results {
        let p = &r.project;
        ps.execute(params![
            p.project_id,
            p.quality_grade,
            p.quality_penalized,
            p.ai_factor,
            p.final_grade,
            p.review_gate,
        ])?;
        for s in &r.students {
            ss.execute(params![
                s.student_id,
                s.project_id,
                s.final_grade,
                s.base_grade,
                s.ai_keep_factor,
                s.contribution_ratio,
                s.student_penalty,
                s.review_gate,
            ])?;
        }
    }
    Ok(())
}

/// Sprint numbers and LLM `target_ref` strings resolved at snapshot build time
/// (same `WorkbookLabels` helpers as the XLSX export).
fn insert_label_lookups(conn: &Connection, data: &WorkbookData) -> Result<()> {
    let mut sprint_stmt =
        conn.prepare("INSERT INTO label_sprint (sprint_id, label) VALUES (?, ?)")?;
    for (id, label) in &data.labels.sprints {
        sprint_stmt.execute(params![id, label])?;
    }

    let mut target_stmt =
        conn.prepare("INSERT INTO label_target (target_ref, label) VALUES (?, ?)")?;
    let mut seen = std::collections::HashSet::new();
    for row in &data.llm_flag_rows {
        if let Some(ref t) = row.target_ref {
            if seen.insert(t.clone()) {
                target_stmt.execute(params![t, data.labels.humanize_target_ref(t)])?;
            }
        }
    }

    let mut task_stmt = conn.prepare("INSERT INTO label_task (task_id, label) VALUES (?, ?)")?;
    for (id, label) in &data.labels.tasks {
        task_stmt.execute(params![id, label])?;
    }
    Ok(())
}

fn create_views(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE VIEW v_student AS
         SELECT s.student_id, s.project_id, s.full_name,
                p.name AS project_name, p.team_size,
                rs.final_grade, rs.base_grade, rs.ai_keep, rs.contribution,
                rs.stu_pen, rs.review_gate
         FROM student s
         JOIN project p ON p.project_id = s.project_id
         LEFT JOIN reference_student rs
                ON rs.student_id = s.student_id AND rs.project_id = s.project_id;

         CREATE VIEW v_team AS
         SELECT p.project_id, p.name, p.team_size,
                rp.quality_grade, rp.quality_penalized, rp.ai_factor,
                rp.final_grade, rp.review_gate,
                (SELECT COUNT(*) FROM student s WHERE s.project_id = p.project_id) AS enrolled
         FROM project p
         LEFT JOIN reference_project rp ON rp.project_id = p.project_id;

         CREATE VIEW v_flag AS
         SELECT p.name AS team, s.full_name AS student, f.source,
                ls.label AS sprint, f.flag_type, f.severity, f.details
         FROM flag f
         JOIN project p ON p.project_id = f.project_id
         JOIN student s ON s.student_id = f.student_id AND s.project_id = f.project_id
         LEFT JOIN label_sprint ls ON ls.sprint_id = f.sprint_id;

         CREATE VIEW v_llm_flag AS
         SELECT p.name AS team, s.full_name AS student, ls.label AS sprint,
                l.scope, COALESCE(lt.label, l.target_ref) AS target,
                l.category, l.severity, l.summary
         FROM llm_flag l
         JOIN project p ON p.project_id = l.project_id
         LEFT JOIN student s
                ON s.student_id = l.student_id AND s.project_id = l.project_id
         LEFT JOIN label_sprint ls ON ls.sprint_id = l.sprint_id
         LEFT JOIN label_target lt ON lt.target_ref = l.target_ref;

         CREATE VIEW v_ai_detect AS
         SELECT p.name AS team, s.full_name AS student, ls.label AS sprint,
                a.risk_level
         FROM ai_detect a
         JOIN project p ON p.project_id = a.project_id
         JOIN student s ON s.student_id = a.student_id AND s.project_id = a.project_id
         LEFT JOIN label_sprint ls ON ls.sprint_id = a.sprint_id;

         CREATE VIEW v_crit_flag AS
         SELECT p.name AS team, c.repo_full_name AS repo, c.kind,
                c.rule_id, c.severity, c.category
         FROM crit_flag c
         JOIN project p ON p.project_id = c.project_id;",
    )
    .context("create snapshot views")?;
    Ok(())
}
