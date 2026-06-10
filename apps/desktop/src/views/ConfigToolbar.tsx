import { useState } from "react";

import {
  APP_CONFIG_FILENAME,
  loadAppConfigFromCwd,
  openAppConfigFile,
  saveAppConfig,
  saveAppConfigAs,
  saveAppConfigToCwd,
} from "../config/appConfig";
import type { GradeSpec, LoadedDb } from "../data/types";

type Props = {
  appConfigPath: string | null;
  loadedDb: LoadedDb | null;
  specPath: string | null;
  onConfigApplied: (result: {
    configPath: string;
    db: LoadedDb | null;
    spec: GradeSpec | null;
    specPath: string | null;
  }) => void;
  onConfigPath: (path: string | null) => void;
};

export default function ConfigToolbar({
  appConfigPath,
  loadedDb,
  specPath,
  onConfigApplied,
  onConfigPath,
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

  const handleSave = async () => {
    setError(null);
    setLoading(true);
    try {
      const dbPath = loadedDb?.path ?? null;
      if (!dbPath && !specPath) {
        setError("Open a grading.db or grading spec before saving configuration");
        return;
      }
      const path = appConfigPath
        ? await saveAppConfig(appConfigPath, dbPath, specPath)
        : await saveAppConfigToCwd(dbPath, specPath);
      onConfigPath(path);
      setError(null);
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
      const dbPath = loadedDb?.path ?? null;
      if (!dbPath && !specPath) {
        setError("Open a grading.db or grading spec before saving configuration");
        return;
      }
      const path = await saveAppConfigAs(dbPath, specPath);
      if (path) onConfigPath(path);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="db-toolbar config-toolbar">
      <button type="button" onClick={() => void handleSave()} disabled={loading}>
        {loading ? "Saving…" : "Save configuration"}
      </button>
      <button type="button" onClick={() => void handleSaveAs()} disabled={loading}>
        Save configuration as…
      </button>
      <button type="button" onClick={() => void handleLoad()} disabled={loading}>
        Load configuration…
      </button>
      <button type="button" className="small" onClick={() => void handleReloadCwd()} disabled={loading}>
        Reload {APP_CONFIG_FILENAME}
      </button>
      {appConfigPath && <span className="meta">Config: {appConfigPath}</span>}
      {error && <span className="error">{error}</span>}
    </div>
  );
}
