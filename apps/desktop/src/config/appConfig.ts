import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { exists, readTextFile, writeTextFile } from "@tauri-apps/plugin-fs";

import type { GradeSpec, LoadedDb } from "../data/types";
import { loadGradingDbFromPath } from "../data/loadGradingDb";
import { parseSpecJson } from "./load";

/** Desktop session file written beside the working directory (or chosen path). */
export const APP_CONFIG_FILENAME = "grader.desktop.json";

export type AppConfig = {
  version: 1;
  grading_db?: string | null;
  grading_spec?: string | null;
};

export type AppliedAppConfig = {
  configPath: string;
  config: AppConfig;
  db: LoadedDb | null;
  spec: GradeSpec | null;
  specPath: string | null;
};

export function parseAppConfig(text: string): AppConfig {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (e) {
    throw new Error(`Invalid app config JSON: ${e instanceof Error ? e.message : String(e)}`);
  }
  if (typeof parsed !== "object" || parsed === null) {
    throw new Error("App config must be a JSON object");
  }
  const obj = parsed as Record<string, unknown>;
  if (obj.version !== 1) {
    throw new Error(`Unsupported app config version: ${String(obj.version)}`);
  }
  const config: AppConfig = { version: 1 };
  if (obj.grading_db != null) {
    if (typeof obj.grading_db !== "string" || obj.grading_db.trim() === "") {
      throw new Error("grading_db must be a non-empty string when set");
    }
    config.grading_db = obj.grading_db;
  }
  if (obj.grading_spec != null) {
    if (typeof obj.grading_spec !== "string" || obj.grading_spec.trim() === "") {
      throw new Error("grading_spec must be a non-empty string when set");
    }
    config.grading_spec = obj.grading_spec;
  }
  return config;
}

export function appConfigToJson(config: AppConfig): string {
  return JSON.stringify(config, null, 2);
}

async function resolveStoredPath(configPath: string, stored: string): Promise<string> {
  return invoke<string>("resolve_stored_path", { configPath, stored });
}

async function relativizePath(configPath: string, absolute: string): Promise<string> {
  return invoke<string>("relativize_path", { configPath, absolute });
}

export async function defaultAppConfigPath(): Promise<string> {
  const cwd = await invoke<string>("get_cwd");
  return invoke<string>("join_path", { baseDir: cwd, fileName: APP_CONFIG_FILENAME });
}

export async function loadAppConfigFromPath(configPath: string): Promise<AppliedAppConfig> {
  const text = await readTextFile(configPath);
  const config = parseAppConfig(text);
  return applyAppConfig(configPath, config);
}

export async function loadAppConfigFromCwd(): Promise<AppliedAppConfig | null> {
  const path = await defaultAppConfigPath();
  if (!(await exists(path))) {
    return null;
  }
  return loadAppConfigFromPath(path);
}

export async function openAppConfigFile(): Promise<AppliedAppConfig | null> {
  const selected = await open({
    multiple: false,
    filters: [{ name: "Grader desktop config", extensions: ["json"] }],
  });
  if (selected === null) return null;
  const path = typeof selected === "string" ? selected : selected;
  return loadAppConfigFromPath(path);
}

export async function saveAppConfig(
  configPath: string,
  gradingDbPath: string | null,
  gradingSpecPath: string | null,
): Promise<string> {
  const config: AppConfig = { version: 1 };
  if (gradingDbPath) {
    config.grading_db = await relativizePath(configPath, gradingDbPath);
  }
  if (gradingSpecPath) {
    config.grading_spec = await relativizePath(configPath, gradingSpecPath);
  }
  await writeTextFile(configPath, appConfigToJson(config));
  return configPath;
}

export async function saveAppConfigAs(
  gradingDbPath: string | null,
  gradingSpecPath: string | null,
  suggestedName = APP_CONFIG_FILENAME,
): Promise<string | null> {
  const path = await save({
    filters: [{ name: "Grader desktop config", extensions: ["json"] }],
    defaultPath: suggestedName,
  });
  if (path === null) return null;
  return saveAppConfig(path, gradingDbPath, gradingSpecPath);
}

export async function saveAppConfigToCwd(
  gradingDbPath: string | null,
  gradingSpecPath: string | null,
): Promise<string> {
  const path = await defaultAppConfigPath();
  return saveAppConfig(path, gradingDbPath, gradingSpecPath);
}

async function applyAppConfig(configPath: string, config: AppConfig): Promise<AppliedAppConfig> {
  let db: LoadedDb | null = null;
  let spec: GradeSpec | null = null;
  let specPath: string | null = null;

  if (config.grading_db) {
    const dbPath = await resolveStoredPath(configPath, config.grading_db);
    if (await exists(dbPath)) {
      db = await loadGradingDbFromPath(dbPath);
    } else {
      throw new Error(`grading.db not found: ${dbPath}`);
    }
  }

  if (config.grading_spec) {
    const resolvedSpecPath = await resolveStoredPath(configPath, config.grading_spec);
    if (await exists(resolvedSpecPath)) {
      const text = await readTextFile(resolvedSpecPath);
      spec = parseSpecJson(text);
      specPath = resolvedSpecPath;
    } else {
      throw new Error(`grading spec not found: ${resolvedSpecPath}`);
    }
  }

  return { configPath, config, db, spec, specPath };
}
