import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { loadBundledDefault } from "../src/config/load";
import { checkParity } from "../src/logic/parity";
import type { GradeOutput } from "../src/data/types";
import { initEngineWithBytes, recomputeCohort } from "../src/engine/index";

const here = dirname(fileURLToPath(import.meta.url));
const wasmPath = join(here, "../pkg/grade_core_wasm_bg.wasm");

describe("parity banner", () => {
  it("reports standard when bundled spec matches reference grades", async () => {
    const spec = loadBundledDefault();
    const rawProjects = JSON.parse(
      readFileSync(join(here, "fixtures/reference.raw_projects.json"), "utf8"),
    );
    await initEngineWithBytes(readFileSync(wasmPath));
    // Cohort grade (as the app does via recomputeAll): v4 bounds + percentile
    // bands are cohort-wide, so a per-project grade would diverge.
    const cohort = await recomputeCohort(rawProjects, spec);
    const grades = new Map<number, GradeOutput>();
    for (const entry of cohort.projects) {
      grades.set(entry.project_id, entry.output);
    }
    const result = checkParity(spec, grades, spec);
    expect(result.state).toBe("standard");
  });

  it("reports edited when spec differs from bundled default", () => {
    const spec = loadBundledDefault();
    const edited = structuredClone(spec);
    edited.weights.w_doc = 0.99;
    const result = checkParity(edited, new Map(), spec);
    expect(result.state).toBe("edited");
  });
});
