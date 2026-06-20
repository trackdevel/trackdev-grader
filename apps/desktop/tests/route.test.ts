import { describe, expect, it } from "vitest";

import {
  nextNavStep,
  parseHash,
  projectHref,
  studentHref,
  topTabOf,
} from "../src/hooks/useHashRoute";

describe("parseHash", () => {
  it("maps empty and unknown hashes to the students tab", () => {
    expect(parseHash("")).toEqual({ page: "students" });
    expect(parseHash("#")).toEqual({ page: "students" });
    expect(parseHash("#/nonsense")).toEqual({ page: "students" });
  });

  it("parses the three top-level tabs", () => {
    expect(parseHash("#/students")).toEqual({ page: "students" });
    expect(parseHash("#/projects")).toEqual({ page: "projects" });
    expect(parseHash("#/formula")).toEqual({ page: "formula" });
  });

  it("parses nested detail routes", () => {
    expect(parseHash("#/students/7/u%40udg.edu")).toEqual({
      page: "student",
      projectId: 7,
      studentId: "u@udg.edu",
    });
    expect(parseHash("#/projects/12")).toEqual({ page: "project", projectId: 12 });
  });

  it("keeps legacy route aliases working", () => {
    expect(parseHash("#/student/7/abc")).toEqual({
      page: "student",
      projectId: 7,
      studentId: "abc",
    });
    expect(parseHash("#/project/12")).toEqual({ page: "project", projectId: 12 });
    expect(parseHash("#/formulas-and-custom-fields")).toEqual({ page: "formula" });
  });

  it("falls back to the list when the project id is not numeric", () => {
    expect(parseHash("#/projects/abc")).toEqual({ page: "projects" });
    expect(parseHash("#/students/abc/def")).toEqual({ page: "students" });
  });

  it("round-trips href helpers", () => {
    expect(parseHash(projectHref(3))).toEqual({ page: "project", projectId: 3 });
    expect(parseHash(studentHref(3, "a/b"))).toEqual({
      page: "student",
      projectId: 3,
      studentId: "a/b",
    });
  });
});

describe("nextNavStep", () => {
  it("advances depth on a fresh forward navigation (unstamped entry)", () => {
    expect(nextNavStep(0, null)).toEqual({ depth: 1, canGoBack: true });
    expect(nextNavStep(3, null)).toEqual({ depth: 4, canGoBack: true });
  });

  it("adopts the stamped depth on a back/forward navigation", () => {
    expect(nextNavStep(4, 1)).toEqual({ depth: 1, canGoBack: true });
  });

  it("reports no back available once we return to the origin entry", () => {
    expect(nextNavStep(1, 0)).toEqual({ depth: 0, canGoBack: false });
  });
});

describe("topTabOf", () => {
  it("groups detail pages under their list tab", () => {
    expect(topTabOf({ page: "student", projectId: 1, studentId: "x" })).toBe("students");
    expect(topTabOf({ page: "project", projectId: 1 })).toBe("projects");
    expect(topTabOf({ page: "formula" })).toBe("formula");
  });
});
