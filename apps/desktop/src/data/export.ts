import { invoke } from "@tauri-apps/api/core";
import { save, open } from "@tauri-apps/plugin-dialog";

import type { GradeOutput, GradeSpec, RawProject } from "./types";

/** Filesystem-safe lowercase slug; mirrors `grade_xlsx::slug` in Rust. */
function slug(name: string): string {
  const s = name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return s.length > 0 ? s : "project";
}

/** Default workbook filename; mirrors `grade_xlsx::grade_workbook_filename`. */
export function gradeWorkbookFilename(projectName: string): string {
  return `notes_${slug(projectName)}.xlsx`;
}

/** `student_id → full_name` from a project's roster. */
function namesOf(raw: RawProject): Record<string, string> {
  const names: Record<string, string> = {};
  for (const s of raw.students) names[s.student_id] = s.full_name;
  return names;
}

/**
 * Write one project's grade workbook to `outPath` via the Rust writer
 * (`grade_xlsx`). This `.xlsx` is the desktop download surface; the CLI emits a
 * Markdown report instead (`grade-md`).
 */
async function writeWorkbook(
  raw: RawProject,
  out: GradeOutput,
  spec: GradeSpec,
  outPath: string,
): Promise<void> {
  await invoke("export_grade_xlsx", {
    payload: {
      out_path: outPath,
      project_name: raw.name,
      names: namesOf(raw),
      grades: out.grades,
      decimals: spec.meta?.decimals ?? 2,
    },
  });
}

/**
 * Export one project: prompt for a destination (defaulting to
 * `notes_<project>.xlsx`) and write it. Returns the path written, or null if the
 * user cancelled the dialog.
 */
export async function exportProjectGrades(
  raw: RawProject,
  out: GradeOutput,
  spec: GradeSpec,
): Promise<string | null> {
  const outPath = await save({
    defaultPath: gradeWorkbookFilename(raw.name),
    filters: [{ name: "Excel", extensions: ["xlsx"] }],
  });
  if (!outPath) return null;
  await writeWorkbook(raw, out, spec, outPath);
  return outPath;
}

/**
 * Export every project that has computed grades into a chosen directory, one
 * `notes_<project>.xlsx` each. Returns the number of files written, or null if
 * the user cancelled the folder picker.
 */
export async function exportAllGrades(
  projects: RawProject[],
  grades: Map<number, GradeOutput>,
  spec: GradeSpec,
): Promise<number | null> {
  const dir = await open({ directory: true, multiple: false });
  if (!dir || typeof dir !== "string") return null;
  let count = 0;
  for (const raw of projects) {
    const out = grades.get(raw.project_id);
    if (!out) continue;
    const outPath = await invoke<string>("join_path", {
      baseDir: dir,
      fileName: gradeWorkbookFilename(raw.name),
    });
    await writeWorkbook(raw, out, spec, outPath);
    count += 1;
  }
  return count;
}
