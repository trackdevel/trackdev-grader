import Database from "@tauri-apps/plugin-sql";

import type { SqlExecutor } from "./types";

export type ProjectRow = {
  id: number;
  name: string;
};

/** Open grading.db read-only via plugin-sql (no journal writes on the user's file). */
export async function openGradingDb(path: string): Promise<Database> {
  const uri = path.startsWith("sqlite:") ? path : `sqlite:${path}?mode=ro`;
  return Database.load(uri);
}

export async function select<T>(
  db: Database,
  sql: string,
  bind: unknown[] = [],
): Promise<T[]> {
  return db.select<T[]>(sql, bind);
}

export async function listProjects(db: Database): Promise<ProjectRow[]> {
  return select<ProjectRow>(
    db,
    "SELECT id, name FROM projects ORDER BY id",
  );
}

/** Adapter for projection.ts against Tauri plugin-sql. */
export function tauriExecutor(db: Database): SqlExecutor {
  return {
    async select<T>(sql: string, bind: unknown[] = []): Promise<T[]> {
      return db.select<T[]>(sql, bind);
    },
    async queryRow<T>(sql: string, bind: unknown[] = []): Promise<T | undefined> {
      const rows = await db.select<T[]>(sql, bind);
      return rows[0];
    },
  };
}
