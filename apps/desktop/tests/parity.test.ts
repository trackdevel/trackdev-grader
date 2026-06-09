import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { loadBundledDefault } from "../src/config/load";
import { checkParity } from "../src/logic/parity";
import type { GradeOutput } from "../src/data/types";
import { initEngineWithBytes, recompute } from "../src/engine/index";

const here = dirname(fileURLToPath(import.meta.url));
const wasmPath = join(here, "../pkg/grade_core_wasm_bg.wasm");

describe("parity banner", () => {
  it("reports standard when bundled spec matches reference grades", async () => {
    const spec = loadBundledDefault();
    const rawProjects = JSON.parse(
      readFileSync(join(here, "fixtures/reference.raw_projects.json"), "utf8"),
    );
    await initEngineWithBytes(readFileSync(wasmPath));
    const grades = new Map<number, GradeOutput>();
    for (const raw of rawProjects) {
      grades.set(raw.project_id, (await recompute(raw, spec)) as GradeOutput);
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
