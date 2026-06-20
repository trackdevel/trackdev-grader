import { describe, expect, it } from "vitest";

import {
  buildLastSession,
  lastSessionToJson,
  parseLastSession,
} from "../src/config/lastSession";

describe("parseLastSession", () => {
  it("accepts config, db, and spec paths", () => {
    const session = parseLastSession(
      JSON.stringify({
        version: 1,
        config_path: "/home/me/grader.desktop.json",
        grading_db: "/home/me/grading.db",
        grading_spec: "/home/me/spec.json",
      }),
    );
    expect(session.config_path).toBe("/home/me/grader.desktop.json");
    expect(session.grading_db).toBe("/home/me/grading.db");
    expect(session.grading_spec).toBe("/home/me/spec.json");
  });

  it("accepts a db-only snapshot", () => {
    expect(parseLastSession(JSON.stringify({ version: 1, grading_db: "/tmp/x.db" }))).toEqual({
      version: 1,
      grading_db: "/tmp/x.db",
    });
  });

  it("rejects unknown versions", () => {
    expect(() => parseLastSession(JSON.stringify({ version: 2, grading_db: "x.db" }))).toThrow(
      /Unsupported/,
    );
  });

  it("rejects an empty snapshot", () => {
    expect(() => parseLastSession(JSON.stringify({ version: 1 }))).toThrow(/at least one path/);
  });

  it("round-trips through lastSessionToJson", () => {
    const raw = lastSessionToJson({
      version: 1,
      grading_db: "/data/grading.db",
    });
    expect(parseLastSession(raw).grading_db).toBe("/data/grading.db");
  });
});

describe("buildLastSession", () => {
  it("returns null when nothing is loaded", () => {
    expect(buildLastSession(null, null, null)).toBeNull();
  });

  it("captures whichever paths were open at close", () => {
    expect(buildLastSession("/cfg.json", "/db.db", "/spec.json")).toEqual({
      version: 1,
      config_path: "/cfg.json",
      grading_db: "/db.db",
      grading_spec: "/spec.json",
    });
  });
});
