import { open, save } from "@tauri-apps/plugin-dialog";
import { readTextFile, writeTextFile } from "@tauri-apps/plugin-fs";

import type { GradeSpec } from "../data/types";
import { validateSpec } from "./validate";

import bundledDefault from "@repo-config/grading.standard.json";

export function loadBundledDefault(): GradeSpec {
  return structuredClone(bundledDefault) as GradeSpec;
}

export function parseSpecJson(text: string): GradeSpec {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (e) {
    throw new Error(`Invalid JSON: ${e instanceof Error ? e.message : String(e)}`);
  }
  const result = validateSpec(parsed as GradeSpec);
  if (!result.ok) {
    throw new Error(result.message);
  }
  const spec = parsed as GradeSpec;
  if (!spec.manual_fields) {
    spec.manual_fields = { defs: [], values: {} };
  }
  return spec;
}

export async function openSpecFile(): Promise<{ spec: GradeSpec; path: string } | null> {
  const selected = await open({
    multiple: false,
    filters: [{ name: "Grading spec JSON", extensions: ["json"] }],
  });
  if (selected === null) return null;
  const path = typeof selected === "string" ? selected : selected;
  const text = await readTextFile(path);
  return { spec: parseSpecJson(text), path };
}

export async function saveSpecAs(
  spec: GradeSpec,
  suggestedName = "grading.custom.json",
): Promise<string | null> {
  const path = await save({
    filters: [{ name: "Grading spec JSON", extensions: ["json"] }],
    defaultPath: suggestedName,
  });
  if (path === null) return null;
  await writeTextFile(path, JSON.stringify(spec, null, 2));
  return path;
}

export async function saveSpecToPath(spec: GradeSpec, path: string): Promise<void> {
  await writeTextFile(path, JSON.stringify(spec, null, 2));
}

export function specToJson(spec: GradeSpec, pretty = true): string {
  return pretty ? JSON.stringify(spec, null, 2) : JSON.stringify(spec);
}

export { bundledDefault as bundledSpecJson };
