import { useCallback, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";

import { listProjects, openGradingDb, tauriExecutor } from "../data/db";
import type { ProjectRow } from "../data/db";
import { loadProjectDiagnostics } from "../data/diagnostics";
import { loadRawProject, sprintIdsUpToCurrent } from "../data/projection";
import type { LoadedDb } from "../data/types";

type Props = {
  onLoaded: (db: LoadedDb) => void;
};

export default function DbPicker({ onLoaded }: Props) {
  const [projectRows, setProjectRows] = useState<ProjectRow[]>([]);
  const [dbPath, setDbPath] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const pickDb = useCallback(async () => {
    setError(null);
    setLoading(true);
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "SQLite database", extensions: ["db"] }],
      });
      if (selected === null) {
        return;
      }
      const path = typeof selected === "string" ? selected : selected;
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
      setDbPath(path);
      setProjectRows(rows);
      onLoaded({ path, projects: rawProjects, diagnostics });
    } catch (e) {
      setProjectRows([]);
      setDbPath(null);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [onLoaded]);

  return (
    <section>
      <button type="button" onClick={() => void pickDb()} disabled={loading}>
        {loading ? "Opening…" : "Open grading.db"}
      </button>
      {dbPath && <p className="meta">Loaded: {dbPath}</p>}
      {error && <p className="error">{error}</p>}
      {projectRows.length > 0 && (
        <table>
          <thead>
            <tr>
              <th>ID</th>
              <th>Name</th>
            </tr>
          </thead>
          <tbody>
            {projectRows.map((p) => (
              <tr key={p.id}>
                <td>{p.id}</td>
                <td>{p.name}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}
