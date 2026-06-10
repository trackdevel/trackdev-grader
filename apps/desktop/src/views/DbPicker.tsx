import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";

import { loadGradingDbFromPath } from "../data/loadGradingDb";
import type { LoadedDb } from "../data/types";

type Props = {
  dbPath: string | null;
  onLoaded: (db: LoadedDb) => void;
};

export default function DbPicker({ dbPath, onLoaded }: Props) {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const pickDb = async () => {
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
      onLoaded(await loadGradingDbFromPath(path));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="db-toolbar">
      <button type="button" onClick={() => void pickDb()} disabled={loading}>
        {loading ? "Opening…" : "Open grading.db"}
      </button>
      {dbPath && <span className="meta">Loaded: {dbPath}</span>}
      {error && <span className="error">{error}</span>}
    </div>
  );
}
