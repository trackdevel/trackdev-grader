import { openGradingDb, listProjects, tauriExecutor } from "./db";
import { loadProjectDiagnostics } from "./diagnostics";
import { loadRawProject, sprintIdsUpToCurrent } from "./projection";
import type { LoadedDb } from "./types";

/** Load all projects from a grading.db path (read-only). */
export async function loadGradingDbFromPath(path: string): Promise<LoadedDb> {
  const db = await openGradingDb(path);
  const exec = tauriExecutor(db);
  const rows = await listProjects(db);
  const today = new Date().toISOString().slice(0, 10);
  const rawProjects = [];
  const diagnostics = new Map<number, Awaited<ReturnType<typeof loadProjectDiagnostics>>>();
  for (const p of rows) {
    const sprintIds = await sprintIdsUpToCurrent(exec, p.id, today);
    rawProjects.push(await loadRawProject(exec, p.id, sprintIds));
    diagnostics.set(p.id, await loadProjectDiagnostics(exec, p.id, sprintIds));
  }
  await db.close();
  return { path, projects: rawProjects, diagnostics };
}
