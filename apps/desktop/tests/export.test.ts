import { describe, expect, it } from "vitest";

import { gradeWorkbookFilename } from "../src/data/export";

// The JS slug must match `grade_xlsx::slug` in Rust so the desktop "export all"
// filenames stay stable across surfaces. These cases mirror the Rust unit tests
// `slug_is_lowercase_and_filesystem_safe` / `filename_uses_slug_and_falls_back`.
describe("gradeWorkbookFilename", () => {
  it("lowercases and replaces unsafe characters", () => {
    expect(gradeWorkbookFilename("Team 01")).toBe("notes_team-01.xlsx");
    expect(gradeWorkbookFilename("equip/Àlfa!")).toBe("notes_equip-lfa.xlsx");
    expect(gradeWorkbookFilename("--a--b--")).toBe("notes_a-b.xlsx");
  });

  it("falls back to 'project' when the slug is empty", () => {
    expect(gradeWorkbookFilename("!!!")).toBe("notes_project.xlsx");
  });
});
