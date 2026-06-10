import { describe, expect, it } from "vitest";

import { APP_CONFIG_FILENAME, appConfigToJson, parseAppConfig } from "../src/config/appConfig";

describe("parseAppConfig", () => {
  it("accepts version 1 with optional paths", () => {
    const config = parseAppConfig(
      JSON.stringify({
        version: 1,
        grading_db: "data/entregues/grading.db",
        grading_spec: "my-spec.json",
      }),
    );
    expect(config).toEqual({
      version: 1,
      grading_db: "data/entregues/grading.db",
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
