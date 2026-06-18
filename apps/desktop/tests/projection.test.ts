import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import Database from "better-sqlite3";
import { describe, expect, it } from "vitest";

import {
  hasGradableArtifact,
  loadRawProject,
  sprintIdsUpToCurrent,
} from "../src/data/projection";
import type {
  GradeOutput,
  ProjectScopes,
  RawProject,
  RepoMetrics,
  SqlExecutor,
  StructuralSpec,
} from "../src/data/types";
import {
  initEngineWithBytes,
  recomputeCohort,
  recomputeStructural,
} from "../src/engine/index";

const here = dirname(fileURLToPath(import.meta.url));
const fixtureDir = join(here, "fixtures");
const repoRoot = join(here, "../../..");
const wasmPath = join(here, "../pkg/grade_core_wasm_bg.wasm");
const TODAY = "2026-03-01";

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

function loadSpec(): StructuralSpec {
  const text = readFileSync(join(repoRoot, "config/grading.standard.json"), "utf8");
  return JSON.parse(text) as StructuralSpec;
}

function close(a: number, b: number, eps = 1e-9): boolean {
  return Math.abs(a - b) < eps;
}

function assertScopesClose(actual: ProjectScopes, expected: ProjectScopes, label: string) {
  expect(close(actual.sum_raw, expected.sum_raw), `${label} sum_raw`).toBe(true);
  expect(close(actual.sum_eff, expected.sum_eff), `${label} sum_eff`).toBe(true);
  expect(close(actual.mean_raw, expected.mean_raw), `${label} mean_raw`).toBe(true);
  expect(close(actual.ai_factor, expected.ai_factor), `${label} ai_factor`).toBe(true);
  expect(actual.students.length).toBe(expected.students.length);
  for (const exp of expected.students) {
    const act = actual.students.find((s) => s.student_id === exp.student_id);
    expect(act, `${label} student ${exp.student_id}`).toBeDefined();
    expect(close(act!.student_eff, exp.student_eff)).toBe(true);
  }
}

describe("loadInventory", () => {
  it("returns empty when repo_structural_metrics table is missing", async () => {
    const db = new Database(":memory:");
    db.exec(`
      CREATE TABLE projects (id INTEGER PRIMARY KEY, name TEXT);
      CREATE TABLE students (id TEXT PRIMARY KEY, full_name TEXT, team_project_id INTEGER);
      CREATE TABLE pull_requests (
        id TEXT PRIMARY KEY, pr_number INTEGER, repo_full_name TEXT,
        url TEXT, title TEXT, state TEXT, merged INTEGER
      );
      CREATE TABLE pr_authors (pr_id TEXT, student_id TEXT);
      CREATE TABLE architecture_violations (repo_full_name TEXT, severity TEXT, rule_kind TEXT);
      CREATE TABLE architecture_runs (repo_full_name TEXT, status TEXT);
      CREATE TABLE student_artifact_flags (
        project_id INTEGER, student_id TEXT, severity TEXT, flag_type TEXT, details TEXT
      );
      INSERT INTO projects VALUES (1, 'T');
      INSERT INTO students VALUES ('s1', 'S', 1);
      INSERT INTO pull_requests VALUES ('pr1', 1, 'spring-api', '', '', 'MERGED', 1);
      INSERT INTO pr_authors VALUES ('pr1', 's1');
    `);
    const exec = makeExecutor(db);
    try {
      const raw = await loadRawProject(exec, 1, []);
      expect(raw.inventory).toEqual([]);
    } finally {
      db.close();
    }
  });
});

describe("hasGradableArtifact", () => {
  function rawWith(inventory: RepoMetrics[]): RawProject {
    return {
      project_id: 1,
      name: "t",
      team_size: 1,
      axis: {
        documentation_raw: 0,
        doc_present: false,
        code_quality_raw: 0,
        cc_pct: 0,
        mutation_score: 0,
        cq_present: false,
        survival_raw: 0,
        surv_present: false,
        arch_crit_count: 0,
        arch_warn_count: 0,
        arch_present: false,
      },
      inventory,
      tasks: [],
      students: [],
      crit_findings: [],
      student_flags: [],
    };
  }

  it("is false without scanned code mass", () => {
    expect(hasGradableArtifact(rawWith([]))).toBe(false);
    // Invariant I1: structural counts alone (no production LOC/statements) don't qualify.
    expect(
      hasGradableArtifact(rawWith([{ repo_full_name: "r", metrics: { endpoint_count: 5 } }])),
    ).toBe(false);
  });

  it("is true with production_loc or production_statement_count", () => {
    expect(
      hasGradableArtifact(rawWith([{ repo_full_name: "r", metrics: { production_loc: 500 } }])),
    ).toBe(true);
    expect(
      hasGradableArtifact(
        rawWith([{ repo_full_name: "r", metrics: { production_statement_count: 10 } }]),
      ),
    ).toBe(true);
  });
});

describe("projection on reference.db", () => {
  const dbPath = join(fixtureDir, "reference.db");
  const rawFixturePath = join(fixtureDir, "reference.raw_projects.json");
  const scopesFixturePath = join(fixtureDir, "reference.scopes.json");
  const projectIds = [1, 2, 3, 4];

  it("loadRawProject matches committed raw fixture", async () => {
    const expected: RawProject[] = JSON.parse(readFileSync(rawFixturePath, "utf8"));
    const db = new Database(dbPath, { readonly: true });
    const exec = makeExecutor(db);
    try {
      for (let i = 0; i < projectIds.length; i += 1) {
        const pid = projectIds[i];
        const sprintIds = await sprintIdsUpToCurrent(exec, pid, TODAY);
        const raw = await loadRawProject(exec, pid, sprintIds);
        expect(raw).toEqual(expected[i]);
      }
    } finally {
      db.close();
    }
  });

  it("WASM structural grade matches committed scopes fixture", async () => {
    const expectedScopes: ProjectScopes[] = JSON.parse(readFileSync(scopesFixturePath, "utf8"));
    const expectedRaw: RawProject[] = JSON.parse(readFileSync(rawFixturePath, "utf8"));
    const spec = loadSpec();
    await initEngineWithBytes(readFileSync(wasmPath));
    for (let i = 0; i < projectIds.length; i += 1) {
      const out = await recomputeStructural(expectedRaw[i], spec);
      assertScopesClose(out.scopes, expectedScopes[i], `project ${projectIds[i]}`);
    }
  });

  it("WASM cohort grade matches reference.grades.json", async () => {
    // v4: axis bounds AND the code-quality percentile bands are cohort-wide, so
    // grade the whole cohort once (as the app does via recomputeAll), not
    // per-project — a cohort-of-1 would use different bounds and percentiles.
    const gradesPath = join(fixtureDir, "reference.grades.json");
    const expected: Array<{
      project: { project_id: number; final_grade: number; quality_grade: number; ai_factor: number };
      students: Array<{ student_id: string; final_grade: number }>;
    }> = JSON.parse(readFileSync(gradesPath, "utf8"));
    const expectedRaw: RawProject[] = JSON.parse(readFileSync(rawFixturePath, "utf8"));
    const spec = loadSpec();
    await initEngineWithBytes(readFileSync(wasmPath));
    const tol = 0.005;
    const cohort = await recomputeCohort(expectedRaw, spec);
    const byId = new Map(cohort.projects.map((p) => [p.project_id, p.output]));
    for (let i = 0; i < projectIds.length; i += 1) {
      const out = byId.get(projectIds[i]) as GradeOutput;
      expect(out, `grade for project ${projectIds[i]}`).toBeDefined();
      const exp = expected[i];
      expect(Math.abs(out.grades.project_final - exp.project.final_grade)).toBeLessThanOrEqual(tol);
      expect(Math.abs(out.grades.quality_grade - exp.project.quality_grade)).toBeLessThanOrEqual(tol);
      for (const expStu of exp.students) {
        const stu = out.grades.students.find((s) => s.student_id === expStu.student_id)!;
        expect(Math.abs(stu.student_final - expStu.final_grade)).toBeLessThanOrEqual(tol);
      }
    }
  });
});

describe("loadRawProject AI sprint gating", () => {
  function seedGatingDb(): Database.Database {
    const db = new Database(":memory:");
    db.exec(readFileSync(join(repoRoot, "crates/core/src/schema.sql"), "utf8"));
    db.exec(`
      INSERT INTO projects (id, slug, name) VALUES (1, 'team-01', 'Team 01');
      INSERT INTO students (id, full_name, team_project_id) VALUES ('alice', 'Alice', 1);
      INSERT INTO sprints (id, project_id, name, start_date, end_date) VALUES
        (100, 1, 'S1', '2026-01-01', '2026-01-15'),
        (200, 1, 'S2', '2026-02-01', '2026-02-15'),
        (300, 1, 'S3', '2026-03-01', '2026-03-15');
      INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id) VALUES
        (1, 'T-1', 'a', 'TASK', 'DONE', 5, 'alice', 100),
        (2, 'T-2', 'b', 'TASK', 'DONE', 7, 'alice', 300);
      INSERT INTO task_ai_usage (task_id, model_value, level_value, declared) VALUES
        (1, 'GPT-5.5', 'E', 1),
        (2, 'GPT-5.5', 'E', 1);
    `);
    return db;
  }

  it("ignores AI declared before the allowed ordinal (sprints 1-2 keep 100%)", async () => {
    const db = seedGatingDb();
    const exec = makeExecutor(db);
    try {
      // ordinal 3 → sprints 1 and 2 are AI-forbidden.
      const raw = await loadRawProject(exec, 1, [100, 200, 300], 3);
      const early = raw.tasks.find((t) => t.raw_points === 5)!;
      const late = raw.tasks.find((t) => t.raw_points === 7)!;
      expect(early.ai_model).toBeNull();
      expect(early.ai_level).toBeNull();
      expect(early.declared).toBe(false);
      expect(early.ai_exempt).toBe(true);
      expect(late.ai_model).toBe("GPT-5.5");
      expect(late.ai_level).toBe("E");
      expect(late.declared).toBe(true);
      expect(late.ai_exempt).toBe(false);
    } finally {
      db.close();
    }
  });

  it("keeps every declaration when ordinal defaults to 1 (no restriction)", async () => {
    const db = seedGatingDb();
    const exec = makeExecutor(db);
    try {
      const raw = await loadRawProject(exec, 1, [100, 200, 300]);
      expect(raw.tasks).toHaveLength(2);
      expect(
        raw.tasks.every((t) => t.declared && t.ai_model === "GPT-5.5" && !t.ai_exempt),
      ).toBe(true);
    } finally {
      db.close();
    }
  });
});

describe("loadRawProject parent USER_STORY AI fallback", () => {
  function seedParentFallbackDb(): Database.Database {
    const db = new Database(":memory:");
    db.exec(readFileSync(join(repoRoot, "crates/core/src/schema.sql"), "utf8"));
    db.exec(`
      INSERT INTO projects (id, slug, name) VALUES (1, 'team-01', 'Team 01');
      INSERT INTO students (id, full_name, team_project_id) VALUES ('alice', 'Alice', 1);
      INSERT INTO sprints (id, project_id, name, start_date, end_date) VALUES
        (300, 1, 'S3', '2026-03-01', '2026-03-15');
      INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id, parent_task_id) VALUES
        (10, 'US-1', 'story',    'USER_STORY', 'DONE', NULL, 'alice', 300, NULL),
        (11, 'T-11', 'inherits', 'TASK',       'DONE', 4,    'alice', 300, 10),
        (12, 'T-12', 'own',      'TASK',       'DONE', 6,    'alice', 300, 10),
        (13, 'T-13', 'orphan',   'TASK',       'DONE', 3,    'alice', 300, NULL);
      INSERT INTO task_ai_usage (task_id, model_value, level_value, declared) VALUES
        (10, 'GPT-5.5', 'E', 1),
        (12, 'Cap', 'A', 1);
    `);
    return db;
  }

  it("an unset task inherits its parent story's AI usage; own attribute wins; orphan stays undeclared", async () => {
    const db = seedParentFallbackDb();
    const exec = makeExecutor(db);
    try {
      // ordinal 1 → no sprint exemption, isolating the parent-fallback logic.
      const raw = await loadRawProject(exec, 1, [300], 1);
      expect(raw.tasks).toHaveLength(3); // USER_STORY parent excluded

      const inherits = raw.tasks.find((t) => t.raw_points === 4)!;
      expect(inherits.ai_model).toBe("GPT-5.5");
      expect(inherits.ai_level).toBe("E");
      expect(inherits.declared).toBe(true);
      expect(inherits.ai_exempt).toBe(false);

      const own = raw.tasks.find((t) => t.raw_points === 6)!;
      expect(own.ai_model).toBe("Cap");
      expect(own.declared).toBe(true);

      const orphan = raw.tasks.find((t) => t.raw_points === 3)!;
      expect(orphan.ai_model).toBeNull();
      expect(orphan.declared).toBe(false);
      expect(orphan.ai_exempt).toBe(false);
    } finally {
      db.close();
    }
  });
});

describe("constants in formulas (WASM)", () => {
  const rawFixturePath = join(fixtureDir, "reference.raw_projects.json");

  it("a constant injected into project_final flows through the engine", async () => {
    const raws: RawProject[] = JSON.parse(readFileSync(rawFixturePath, "utf8"));
    const spec = loadSpec();
    spec.constants = [{ name: "flat_grade", value: 0, description: "" }];
    // Wire project_final straight to the constant so the effect is unambiguous.
    const idx = spec.formulas.project.findIndex((f) => f.name === "project_final");
    spec.formulas.project[idx] = {
      name: "project_final",
      infix: "flat_grade",
      expr: { op: "var", name: "flat_grade" },
    };
    await initEngineWithBytes(readFileSync(wasmPath));

    const zero = await recomputeCohort(raws, spec);
    expect(zero.projects[0].output.grades.project_final).toBeCloseTo(0, 6);

    spec.constants[0].value = 5;
    const five = await recomputeCohort(raws, spec);
    expect(five.projects[0].output.grades.project_final).toBeCloseTo(5, 6);
  });

  it("manual-field explanation notes are ignored by the engine (grades unchanged)", async () => {
    const raws: RawProject[] = JSON.parse(readFileSync(rawFixturePath, "utf8"));
    const spec = loadSpec();
    await initEngineWithBytes(readFileSync(wasmPath));
    const before = (await recomputeCohort(raws, spec)).projects[0].output.grades.project_final;
    spec.manual_fields = {
      defs: [],
      values: {},
      notes: { "1": { anything: "a multiline\nexplanation" } },
    };
    const after = (await recomputeCohort(raws, spec)).projects[0].output.grades.project_final;
    expect(after).toBeCloseTo(before, 9);
  });
});
