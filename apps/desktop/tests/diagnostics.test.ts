import Database from "better-sqlite3";
import { describe, expect, it } from "vitest";

import { loadProjectDiagnostics } from "../src/data/diagnostics";
import type { SqlExecutor } from "../src/data/types";

function makeExecutor(db: Database.Database): SqlExecutor {
  return {
    async select<T>(sql: string, bind: unknown[] = []): Promise<T[]> {
      return db.prepare(sql).all(...bind) as T[];
    },
    async queryRow<T>(sql: string, bind: unknown[] = []): Promise<T | undefined> {
      return db.prepare(sql).get(...bind) as T | undefined;
    },
  };
}

/** Minimal schema covering every table loadProjectDiagnostics touches. */
function seedDb(): Database.Database {
  const db = new Database(":memory:");
  db.exec(`
    CREATE TABLE projects (id INTEGER PRIMARY KEY, name TEXT);
    CREATE TABLE students (id TEXT PRIMARY KEY, full_name TEXT, team_project_id INTEGER);
    CREATE TABLE sprints (id INTEGER PRIMARY KEY, name TEXT, project_id INTEGER, start_date TEXT);
    CREATE TABLE tasks (
      id INTEGER PRIMARY KEY, task_key TEXT, name TEXT, type TEXT, status TEXT,
      estimation_points INTEGER, assignee_id TEXT, sprint_id INTEGER
    );
    CREATE TABLE task_ai_usage (
      task_id INTEGER PRIMARY KEY, model_value TEXT, level_value TEXT, declared INTEGER
    );
    CREATE TABLE flags (student_id TEXT, flag_type TEXT, severity TEXT, details TEXT, sprint_id INTEGER);
    CREATE TABLE student_artifact_flags (
      project_id INTEGER, student_id TEXT, flag_type TEXT, severity TEXT, details TEXT
    );
    CREATE TABLE student_sprint_ai_usage (
      project_id INTEGER, student_id TEXT, sprint_id INTEGER, risk_level TEXT
    );

    INSERT INTO projects VALUES (1, 'Team');
    INSERT INTO students VALUES ('alice', 'Alice', 1);
    INSERT INTO sprints VALUES (10, 'S1', 1, '2026-01-01'), (20, 'S2', 1, '2026-02-01');

    -- DONE TASK in the later sprint, AI declared.
    INSERT INTO tasks VALUES (2, 'T-2', 'b', 'TASK', 'DONE', 3, 'alice', 20);
    INSERT INTO task_ai_usage VALUES (2, 'GPT', 'B', 1);
    -- DONE TASK in the earlier sprint, no AI declaration row.
    INSERT INTO tasks VALUES (1, 'T-1', 'a', 'TASK', 'DONE', 5, 'alice', 10);
    -- USER_STORY must be excluded even when DONE.
    INSERT INTO tasks VALUES (3, 'US-1', 'us', 'USER_STORY', 'DONE', 8, 'alice', 10);
    -- Non-DONE task must be excluded.
    INSERT INTO tasks VALUES (4, 'T-4', 'd', 'TASK', 'TODO', 2, 'alice', 20);
  `);
  return db;
}

describe("loadProjectDiagnostics display tasks", () => {
  it("returns DONE non-USER_STORY tasks ordered by sprint then id, with AI + sprint enrichment", async () => {
    const db = seedDb();
    try {
      const diag = await loadProjectDiagnostics(makeExecutor(db), 1, [10, 20]);
      expect(diag.tasks.map((t) => t.task_key)).toEqual(["T-1", "T-2"]);

      const [first, second] = diag.tasks;
      // Earliest sprint first; AI cells empty because the feature wasn't used yet.
      expect(first).toMatchObject({
        task_id: 1,
        task_key: "T-1",
        sprint: "S1",
        raw_points: 5,
        ai_model: null,
        ai_level: null,
        declared: false,
      });
      // Later sprint surfaces the declared model/level.
      expect(second).toMatchObject({
        task_id: 2,
        task_key: "T-2",
        sprint: "S2",
        raw_points: 3,
        ai_model: "GPT",
        ai_level: "B",
        declared: true,
      });
    } finally {
      db.close();
    }
  });

  it("returns no tasks when the sprint window is empty", async () => {
    const db = seedDb();
    try {
      const diag = await loadProjectDiagnostics(makeExecutor(db), 1, []);
      expect(diag.tasks).toEqual([]);
    } finally {
      db.close();
    }
  });
});
