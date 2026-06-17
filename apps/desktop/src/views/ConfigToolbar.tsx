import { useEffect, useRef, useState } from "react";

import {
  APP_CONFIG_FILENAME,
  defaultAppConfigPath,
  defaultSpecPathFor,
  loadAppConfigFromCwd,
  openAppConfigFile,
  planSpecFlush,
  promptConfigPath,
  saveAppConfig,
} from "../config/appConfig";
import { saveSpecToPath } from "../config/load";
import type { GradeSpec, LoadedDb } from "../data/types";

type Props = {
  appConfigPath: string | null;
  loadedDb: LoadedDb | null;
  spec: GradeSpec;
  edited: boolean;
  dirty: boolean;
  specPath: string | null;
  onConfigApplied: (result: {
    configPath: string;
    db: LoadedDb | null;
    spec: GradeSpec | null;
    specPath: string | null;
  }) => void;
  onConfigPath: (path: string | null) => void;
  onSpecPath: (path: string | null) => void;
  onSaved: () => void;
};

export default function ConfigToolbar({
  appConfigPath,
  loadedDb,
  spec,
  edited,
  dirty,
  specPath,
  onConfigApplied,
  onConfigPath,
  onSpecPath,
  onSaved,
}: Props) {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const applyLoaded = (result: Awaited<ReturnType<typeof openAppConfigFile>>) => {
    if (!result) return;
    onConfigPath(result.configPath);
    onConfigApplied({
      configPath: result.configPath,
      db: result.db,
      spec: result.spec,
      specPath: result.specPath,
    });
    setError(null);
  };

  const handleLoad = async () => {
    setError(null);
    setLoading(true);
    try {
      applyLoaded(await openAppConfigFile());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  const handleReloadCwd = async () => {
    setError(null);
    setLoading(true);
    try {
      const result = await loadAppConfigFromCwd();
      if (result) {
        applyLoaded(result);
      } else {
        setError(`No ${APP_CONFIG_FILENAME} in the current working directory`);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  /**
   * Flush the in-memory spec to disk so the session pointer references current
   * formulas + custom fields rather than a stale file, and return the spec path
   * to record. A never-saved (bundled-default) spec is written to a default
   * file beside the session — no Save-As dialog, so the user is never asked
   * "which file?" and can't accidentally aim the pointer at the config itself.
   */
  const flushSpec = async (configPath: string): Promise<string | null> => {
    const plan = planSpecFlush(specPath, edited);
    if (plan.action === "write") {
      await saveSpecToPath(spec, plan.path);
      return plan.path;
    }
    if (plan.action === "write-default") {
      const target = await defaultSpecPathFor(configPath);
      await saveSpecToPath(spec, target);
      onSpecPath(target);
      return target;
    }
    return specPath;
  };

  /** Save everything (spec + session pointer) to a single destination. */
  const persist = async (configPath: string) => {
    const dbPath = loadedDb?.path ?? null;
    const flushed = await flushSpec(configPath);
    if (!dbPath && !flushed) {
      setError("Open a grading.db or edit the grading spec before saving");
      return;
    }
    await saveAppConfig(configPath, dbPath, flushed);
    onConfigPath(configPath);
    onSaved();
    setError(null);
  };

  const handleSave = async () => {
    setError(null);
    setLoading(true);
    try {
      await persist(appConfigPath ?? (await defaultAppConfigPath()));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  const handleSaveAs = async () => {
    setError(null);
    setLoading(true);
    try {
      const chosen = await promptConfigPath();
      if (chosen) await persist(chosen);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  // Ctrl/Cmd-S triggers the one Save. The ref always holds the latest closure
  // so the listener sees current props without re-binding every render.
  const saveRef = useRef<() => void>(() => {});
  saveRef.current = () => {
    if (!loading) void handleSave();
  };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && (e.key === "s" || e.key === "S")) {
        e.preventDefault();
        saveRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="db-toolbar config-toolbar">
      <button
        type="button"
        onClick={() => void handleSave()}
        disabled={loading}
        title="Save formulas, custom fields, and the database/spec paths (Ctrl/Cmd-S)"
      >
        {loading ? "Saving…" : "Save"}
      </button>
      <button type="button" onClick={() => void handleSaveAs()} disabled={loading}>
        Save as…
      </button>
      <button type="button" onClick={() => void handleLoad()} disabled={loading}>
        Load…
      </button>
      <button type="button" className="small" onClick={() => void handleReloadCwd()} disabled={loading}>
        Reload {APP_CONFIG_FILENAME}
      </button>
      {dirty && (
        <span className="badge edited" title="Unsaved changes">
          ● unsaved
        </span>
      )}
      {appConfigPath && <span className="meta">Config: {appConfigPath}</span>}
      {error && <span className="error">{error}</span>}
    </div>
  );
}
