import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import Database from "better-sqlite3";
import { describe, expect, it } from "vitest";

import {
  loadRawProject,
  sprintIdsUpToCurrent,
} from "../src/data/projection";
import type {
  GradeOutput,
  ProjectScopes,
  RawProject,
  SqlExecutor,
  StructuralSpec,
} from "../src/data/types";
import { initEngineWithBytes, recompute, recomputeStructural } from "../src/engine/index";

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

  it("WASM full grade matches reference.grades.json", async () => {
    const gradesPath = join(fixtureDir, "reference.grades.json");
    const expected: Array<{
      project: { final_grade: number; quality_grade: number; ai_factor: number };
      students: Array<{ student_id: string; final_grade: number }>;
    }> = JSON.parse(readFileSync(gradesPath, "utf8"));
    const expectedRaw: RawProject[] = JSON.parse(readFileSync(rawFixturePath, "utf8"));
    const spec = loadSpec();
    await initEngineWithBytes(readFileSync(wasmPath));
    const tol = 0.005;
    for (let i = 0; i < projectIds.length; i += 1) {
      const out = (await recompute(expectedRaw[i], spec)) as GradeOutput;
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
