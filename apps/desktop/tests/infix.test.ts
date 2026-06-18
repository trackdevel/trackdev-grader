import { describe, expect, it } from "vitest";

import { exprToInfix, parseInfix, type Expr } from "../src/config/infix";
import { freeVarsFromExpr } from "../src/config/expr";
import bundled from "@repo-config/grading.standard.json";

/** Reference evaluator mirroring grade_core::formula::eval_inner. */
function evalExpr(e: Expr, scope: Map<string, number>): number {
  switch (e.op) {
    case "num":
      return e.value;
    case "var": {
      const v = scope.get(e.name);
      if (v === undefined) throw new Error(`unknown var ${e.name}`);
      return v;
    }
    case "add":
      return e.terms.reduce((acc, t) => acc + evalExpr(t, scope), 0);
    case "sub":
      return evalExpr(e.lhs, scope) - evalExpr(e.rhs, scope);
    case "mul":
      return e.factors.reduce((acc, f) => acc * evalExpr(f, scope), 1);
    case "div":
      return evalExpr(e.num, scope) / evalExpr(e.den, scope);
    case "min":
      return Math.min(...e.args.map((a) => evalExpr(a, scope)));
    case "max":
      return Math.max(...e.args.map((a) => evalExpr(a, scope)));
    case "clamp": {
      const x = evalExpr(e.x, scope);
      const lo = evalExpr(e.lo, scope);
      const hi = evalExpr(e.hi, scope);
      return Math.min(Math.max(x, lo), hi);
    }
    case "pow":
      return Math.pow(evalExpr(e.base, scope), evalExpr(e.exp, scope));
  }
}

/** Deterministic pseudo-random scope so failures reproduce. */
function scopeFor(vars: Set<string>, salt: number): Map<string, number> {
  const out = new Map<string, number>();
  let state = 0x9e3779b9 ^ salt;
  for (const name of [...vars].sort()) {
    state = (state * 1664525 + 1013904223) >>> 0;
    // Strictly positive values avoid div-by-zero in denominator vars.
    out.set(name, 0.1 + (state % 1000) / 100);
  }
  return out;
}

describe("parseInfix", () => {
  it("parses literals, vars and operator precedence", () => {
    expect(parseInfix("3")).toEqual({ op: "num", value: 3 });
    expect(parseInfix("x")).toEqual({ op: "var", name: "x" });
    expect(parseInfix("a + b * c")).toEqual({
      op: "add",
      terms: [
        { op: "var", name: "a" },
        { op: "mul", factors: [{ op: "var", name: "b" }, { op: "var", name: "c" }] },
      ],
    });
    expect(parseInfix("(a + b) * c")).toEqual({
      op: "mul",
      factors: [
        { op: "add", terms: [{ op: "var", name: "a" }, { op: "var", name: "b" }] },
        { op: "var", name: "c" },
      ],
    });
  });

  it("flattens chained additions and multiplications", () => {
    expect(parseInfix("a + b + c")).toEqual({
      op: "add",
      terms: [
        { op: "var", name: "a" },
        { op: "var", name: "b" },
        { op: "var", name: "c" },
      ],
    });
    expect(parseInfix("a * b * c")).toEqual({
      op: "mul",
      factors: [
        { op: "var", name: "a" },
        { op: "var", name: "b" },
        { op: "var", name: "c" },
      ],
    });
  });

  it("keeps subtraction and division left-associative", () => {
    const e = parseInfix("10 - 2 - 3");
    expect(evalExpr(e, new Map())).toBe(5);
    const d = parseInfix("12 / 2 / 3");
    expect(evalExpr(d, new Map())).toBe(2);
  });

  it("handles unary minus", () => {
    expect(parseInfix("-3")).toEqual({ op: "num", value: -3 });
    expect(evalExpr(parseInfix("-x + 5"), new Map([["x", 2]]))).toBe(3);
  });

  it("parses min/max/clamp", () => {
    expect(evalExpr(parseInfix("min(3, 8)"), new Map())).toBe(3);
    expect(evalExpr(parseInfix("max(3, 8, 2)"), new Map())).toBe(8);
    expect(evalExpr(parseInfix("clamp(15, 0, 10)"), new Map())).toBe(10);
  });

  it("rejects malformed input with a position", () => {
    expect(() => parseInfix("a +")).toThrow(/end of formula/i);
    expect(() => parseInfix("a b")).toThrow(/position/);
    expect(() => parseInfix("foo(1)")).toThrow(/Unknown function 'foo'/);
    expect(() => parseInfix("clamp(1, 2)")).toThrow(/exactly 3/);
    expect(() => parseInfix("min(1)")).toThrow(/at least 2/);
    expect(() => parseInfix("(a + b")).toThrow(/Expected '\)'/);
    expect(() => parseInfix("a # b")).toThrow(/Unexpected character/);
  });

  it("round-trips through exprToInfix", () => {
    for (const src of [
      "a + b * c",
      "(a - b) / (c + d)",
      "clamp(10 * x / y, 0, 10)",
      "1 - (1 - floor_keep) * ai_strength",
      "min(cap, bonus * score) + max(a, b)",
    ]) {
      const ast = parseInfix(src);
      const reprinted = exprToInfix(ast);
      expect(parseInfix(reprinted)).toEqual(ast);
    }
  });
});

describe("bundled spec consistency", () => {
  type Formulas = Record<string, Array<{ name: string; infix: string; expr: unknown }>>;
  const formulas = bundled.formulas as unknown as Formulas;

  for (const scope of ["task", "project", "student"]) {
    for (const fd of formulas[scope]) {
      it(`${scope}.${fd.name}: parsed infix is numerically equivalent to the stored AST`, () => {
        const parsed = parseInfix(fd.infix);
        const stored = fd.expr as Expr;
        expect(freeVarsFromExpr(parsed)).toEqual(freeVarsFromExpr(stored));
        const vars = freeVarsFromExpr(stored);
        for (let salt = 0; salt < 5; salt += 1) {
          const env = scopeFor(vars, salt);
          expect(evalExpr(parsed, env)).toBeCloseTo(evalExpr(stored, env), 9);
        }
      });
    }
  }
});
