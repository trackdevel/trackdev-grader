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
