import { describe, expect, it } from "vitest";

import { isAiUsageSet, resolveEffectiveAiUsage } from "../src/data/taskAi";

describe("resolveEffectiveAiUsage", () => {
  const unset = { model_value: null, level_value: null, declared: null };
  const parent = { model_value: "GPT-5.5", level_value: "E", declared: 1 };

  it("prefers own attribute over parent", () => {
    const own = { model_value: "Cap", level_value: "A", declared: 1 };
    expect(resolveEffectiveAiUsage(own, parent)).toEqual({
      ai_model: "Cap",
      ai_level: "A",
      declared: true,
    });
  });

  it("inherits parent when own is unset", () => {
    expect(resolveEffectiveAiUsage(unset, parent)).toEqual({
      ai_model: "GPT-5.5",
      ai_level: "E",
      declared: true,
    });
  });

  it("treats partial own rows as unset (both-present gate)", () => {
    expect(
      resolveEffectiveAiUsage({ model_value: "Cap", level_value: null, declared: 1 }, parent),
    ).toEqual({
      ai_model: "GPT-5.5",
      ai_level: "E",
      declared: true,
    });
  });

  it("returns undeclared when neither own nor parent is set", () => {
    expect(resolveEffectiveAiUsage(unset, unset)).toEqual({
      ai_model: null,
      ai_level: null,
      declared: false,
    });
  });
});

describe("isAiUsageSet", () => {
  it("requires declared flag plus model and level", () => {
    expect(isAiUsageSet("Cap", "A", 1)).toBe(true);
    expect(isAiUsageSet("Cap", null, 1)).toBe(false);
    expect(isAiUsageSet(null, "A", 1)).toBe(false);
    expect(isAiUsageSet("Cap", "A", 0)).toBe(false);
  });
});
