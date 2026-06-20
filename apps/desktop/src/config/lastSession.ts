import { invoke } from "@tauri-apps/api/core";
import { exists, readTextFile } from "@tauri-apps/plugin-fs";

import { loadGradingDbFromPath } from "../data/loadGradingDb";
import type { GradeSpec, LoadedDb } from "../data/types";
import { loadAppConfigFromPath } from "./appConfig";
import { parseSpecJson } from "./load";

/** Persisted in the Tauri app-data dir when the window closes with data loaded. */
export type LastSession = {
  version: 1;
  config_path?: string;
  grading_db?: string;
  grading_spec?: string;
};

export type RestoredSession = {
  configPath: string | null;
  db: LoadedDb | null;
  spec: GradeSpec | null;
  specPath: string | null;
};

export function parseLastSession(text: string): LastSession {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (e) {
    throw new Error(`Invalid last-session JSON: ${e instanceof Error ? e.message : String(e)}`);
  }
  if (typeof parsed !== "object" || parsed === null) {
    throw new Error("Last session must be a JSON object");
  }
  const obj = parsed as Record<string, unknown>;
  if (obj.version !== 1) {
    throw new Error(`Unsupported last-session version: ${String(obj.version)}`);
  }
  const session: LastSession = { version: 1 };
  for (const key of ["config_path", "grading_db", "grading_spec"] as const) {
    const value = obj[key];
    if (value == null) continue;
    if (typeof value !== "string" || value.trim() === "") {
      throw new Error(`${key} must be a non-empty string when set`);
    }
    session[key] = value;
  }
  if (!session.config_path && !session.grading_db && !session.grading_spec) {
    throw new Error("Last session must reference at least one path");
  }
  return session;
}

export function lastSessionToJson(session: LastSession): string {
  return JSON.stringify(session, null, 2);
}

/** Build the snapshot written on window close; null when nothing was loaded. */
export function buildLastSession(
  configPath: string | null,
  dbPath: string | null,
  specPath: string | null,
): LastSession | null {
  if (!configPath && !dbPath && !specPath) {
    return null;
  }
  const session: LastSession = { version: 1 };
  if (configPath) session.config_path = configPath;
  if (dbPath) session.grading_db = dbPath;
  if (specPath) session.grading_spec = specPath;
  return session;
}

export async function persistLastSession(session: LastSession | null): Promise<void> {
  await invoke("write_last_session", {
    payload: session ? lastSessionToJson(session) : null,
  });
}

export async function readLastSession(): Promise<LastSession | null> {
  const text = await invoke<string | null>("read_last_session");
  if (text === null) return null;
  return parseLastSession(text);
}

/**
 * Reload the session saved when the app last closed with a db and/or config
 * open. Returns null when no snapshot exists or every referenced file is gone.
 */
export async function restoreFromLastSession(): Promise<RestoredSession | null> {
  const last = await readLastSession();
  if (!last) return null;

  if (last.config_path && (await exists(last.config_path))) {
    try {
      const applied = await loadAppConfigFromPath(last.config_path);
      return {
        configPath: applied.configPath,
        db: applied.db,
        spec: applied.spec,
        specPath: applied.specPath,
      };
    } catch {
      // Fall through to the absolute paths captured alongside the config pointer.
    }
  }

  let db: LoadedDb | null = null;
  let spec: GradeSpec | null = null;
  let specPath: string | null = null;

  if (last.grading_db && (await exists(last.grading_db))) {
    db = await loadGradingDbFromPath(last.grading_db);
  }
  if (last.grading_spec && (await exists(last.grading_spec))) {
    const text = await readTextFile(last.grading_spec);
    spec = parseSpecJson(text);
    specPath = last.grading_spec;
  }

  if (!db && !spec) {
    return null;
  }

  return {
    configPath: last.config_path ?? null,
    db,
    spec,
    specPath,
  };
}
