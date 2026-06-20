import { describe, expect, it } from "vitest";

import {
  parseStoredSort,
  readStoredSort,
  resolveInitialSort,
  writeStoredSort,
  type SortStorage,
} from "../src/views/tableSort";

function fakeStorage(): SortStorage & { map: Map<string, string> } {
  const map = new Map<string, string>();
  return {
    map,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => {
      map.set(k, v);
    },
  };
}

describe("parseStoredSort", () => {
  it("accepts a well-formed sort", () => {
    expect(parseStoredSort('{"key":"grade","dir":"desc"}')).toEqual({
      key: "grade",
      dir: "desc",
    });
  });

  it("rejects null, junk, and bad shapes", () => {
    expect(parseStoredSort(null)).toBeNull();
    expect(parseStoredSort("not json")).toBeNull();
    expect(parseStoredSort('{"key":"x","dir":"sideways"}')).toBeNull();
    expect(parseStoredSort('{"dir":"asc"}')).toBeNull();
  });
});

describe("read/write round-trip", () => {
  it("persists and reads back the chosen ordering", () => {
    const storage = fakeStorage();
    writeStoredSort(storage, "students", { key: "team", dir: "desc" });
    expect(readStoredSort(storage, "students")).toEqual({ key: "team", dir: "desc" });
  });

  it("namespaces by id so tables do not collide", () => {
    const storage = fakeStorage();
    writeStoredSort(storage, "students", { key: "team", dir: "asc" });
    writeStoredSort(storage, "projects", { key: "grade", dir: "desc" });
    expect(readStoredSort(storage, "students")).toEqual({ key: "team", dir: "asc" });
    expect(readStoredSort(storage, "projects")).toEqual({ key: "grade", dir: "desc" });
  });

  it("is a no-op when storage is unavailable", () => {
    expect(() => writeStoredSort(null, "students", { key: "team", dir: "asc" })).not.toThrow();
    expect(readStoredSort(null, "students")).toBeNull();
  });
});

describe("resolveInitialSort", () => {
  const keys = ["team", "grade", "base"];
  const fallback = { key: "grade", dir: "desc" as const };

  it("prefers a stored sort whose column still exists", () => {
    expect(resolveInitialSort({ key: "team", dir: "asc" }, fallback, keys)).toEqual({
      key: "team",
      dir: "asc",
    });
  });

  it("ignores a stored sort whose column was removed", () => {
    expect(resolveInitialSort({ key: "gone", dir: "asc" }, fallback, keys)).toEqual(fallback);
  });

  it("falls back to the default when nothing is stored", () => {
    expect(resolveInitialSort(null, fallback, keys)).toEqual(fallback);
  });

  it("falls back to the first column when there is no default", () => {
    expect(resolveInitialSort(null, undefined, keys)).toEqual({ key: "team", dir: "asc" });
  });
});
