//! Human-readable labels for grading workbook export (no internal TrackDev ids).

use std::collections::HashMap;

use rusqlite::Connection;

#[derive(Debug, Clone, Default)]
pub struct WorkbookLabels {
    pub projects: HashMap<i64, String>,
    pub students: HashMap<String, String>,
    pub sprints: HashMap<i64, String>,
    pub tasks: HashMap<i64, String>,
}

impl WorkbookLabels {
    pub fn load(conn: &Connection) -> rusqlite::Result<Self> {
        let mut projects = HashMap::new();
        let mut stmt = conn.prepare("SELECT id, name FROM projects")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, name) = row?;
            projects.insert(id, name);
        }

        let mut students = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT id, full_name, username FROM students WHERE team_project_id IS NOT NULL",
        )?;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (id, full_name, username) = row?;
            let label = if full_name.trim().is_empty() {
                username
            } else {
                full_name
            };
            students.insert(id, label);
        }

        let mut sprints = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, name, start_date FROM sprints ORDER BY project_id, start_date",
        )?;
        let mut ordinal_by_project: HashMap<i64, u32> = HashMap::new();
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (id, project_id, name) = row?;
            let ord = ordinal_by_project.entry(project_id).or_insert(0);
            *ord += 1;
            let label = parse_sprint_number_from_name(&name)
                .map(|n| n.to_string())
                .unwrap_or_else(|| ord.to_string());
            sprints.insert(id, label);
        }

        let mut tasks = HashMap::new();
        let mut stmt = conn.prepare("SELECT id, task_key FROM tasks WHERE type != 'USER_STORY'")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, key) = row?;
            tasks.insert(id, key);
        }

        Ok(Self {
            projects,
            students,
            sprints,
            tasks,
        })
    }

    pub fn project(&self, id: i64) -> String {
        self.projects
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("project-{id}"))
    }

    pub fn student(&self, id: &str) -> String {
        self.students
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    }

    pub fn sprint(&self, id: i64) -> String {
        self.sprints
            .get(&id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    }

    pub fn task(&self, id: i64) -> String {
        self.tasks
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("task-{id}"))
    }

    /// Rewrite `project:{id}` refs to `project:{name}` for LLM flag rows.
    pub fn humanize_target_ref(&self, target: &str) -> String {
        if let Some(rest) = target.strip_prefix("project:") {
            if let Ok(pid) = rest.parse::<i64>() {
                return format!("project:{}", self.project(pid));
            }
        }
        target.to_string()
    }
}

/// Pull a sprint index from names like `Sprint 1`, `S1`, `sprint-2`.
fn parse_sprint_number_from_name(name: &str) -> Option<u8> {
    let digits: String = name.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let n: u8 = digits.parse().ok()?;
    (1..=4).contains(&n).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE projects (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE students (id TEXT PRIMARY KEY, full_name TEXT, username TEXT, team_project_id INTEGER);
             CREATE TABLE sprints (id INTEGER PRIMARY KEY, project_id INTEGER, name TEXT, start_date TEXT);
             CREATE TABLE tasks (id INTEGER PRIMARY KEY, task_key TEXT, type TEXT);
             INSERT INTO projects VALUES (1, 'pds26-1a');
             INSERT INTO students VALUES ('u-99', 'Ada Lovelace', 'ada', 1);
             INSERT INTO sprints VALUES (10, 1, 'Sprint 1', '2026-01-01');
             INSERT INTO sprints VALUES (11, 1, 'Sprint 2', '2026-01-15');
             INSERT INTO tasks VALUES (5, 'T-101', 'TASK');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn labels_resolve_names_and_sprint_numbers() {
        let labels = WorkbookLabels::load(&conn()).unwrap();
        assert_eq!(labels.project(1), "pds26-1a");
        assert_eq!(labels.student("u-99"), "Ada Lovelace");
        assert_eq!(labels.sprint(10), "1");
        assert_eq!(labels.sprint(11), "2");
        assert_eq!(labels.task(5), "T-101");
        assert_eq!(labels.humanize_target_ref("project:1"), "project:pds26-1a");
    }
}
