import { describe, expect, it } from "vitest";

import {
  APP_CONFIG_FILENAME,
  appConfigToJson,
  parseAppConfig,
  planSpecFlush,
} from "../src/config/appConfig";

describe("parseAppConfig", () => {
  it("accepts version 1 with optional paths", () => {
    const config = parseAppConfig(
      JSON.stringify({
        version: 1,
        grading_db: "data/grading.db",
        grading_spec: "my-spec.json",
      }),
    );
    expect(config).toEqual({
      version: 1,
      grading_db: "data/grading.db",
      grading_spec: "my-spec.json",
    });
  });

  it("accepts an empty session (version only)", () => {
    expect(parseAppConfig(JSON.stringify({ version: 1 }))).toEqual({ version: 1 });
  });

  it("rejects unknown versions", () => {
    expect(() => parseAppConfig(JSON.stringify({ version: 2 }))).toThrow(/Unsupported/);
  });

  it("round-trips through appConfigToJson", () => {
    const raw = appConfigToJson({
      version: 1,
      grading_db: "grading.db",
    });
    expect(parseAppConfig(raw).grading_db).toBe("grading.db");
  });

  it("exports the cwd default filename", () => {
    expect(APP_CONFIG_FILENAME).toBe("grader.desktop.json");
  });
});

describe("planSpecFlush", () => {
  it("overwrites the open spec file in place", () => {
    expect(planSpecFlush("config/grading.custom.json", true)).toEqual({
      action: "write",
      path: "config/grading.custom.json",
    });
  });

  it("rewrites the open spec file even when unedited (keeps it current)", () => {
    expect(planSpecFlush("config/grading.custom.json", false)).toEqual({
      action: "write",
      path: "config/grading.custom.json",
    });
  });

  it("writes an edited file-less spec to a default path (no Save-As prompt)", () => {
    expect(planSpecFlush(null, true)).toEqual({ action: "write-default" });
  });

  it("does nothing for the unedited bundled default with no file", () => {
    expect(planSpecFlush(null, false)).toEqual({ action: "none" });
  });
});
