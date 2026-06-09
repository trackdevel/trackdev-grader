/** Collect free variable names from a formula `Expr` JSON value. */

type ExprJson = {
  op: string;
  value?: number;
  name?: string;
  terms?: ExprJson[];
  factors?: ExprJson[];
  args?: ExprJson[];
  lhs?: ExprJson;
  rhs?: ExprJson;
  num?: ExprJson;
  den?: ExprJson;
  x?: ExprJson;
  lo?: ExprJson;
  hi?: ExprJson;
};

export function freeVarsFromExpr(expr: unknown): Set<string> {
  const out = new Set<string>();
  walkExpr(expr, out);
  return out;
}

function walkExpr(expr: unknown, out: Set<string>): void {
  if (!expr || typeof expr !== "object") return;
  const e = expr as ExprJson;
  switch (e.op) {
    case "var":
      if (e.name) out.add(e.name);
      break;
    case "num":
      break;
    case "add":
      e.terms?.forEach((t) => walkExpr(t, out));
      break;
    case "sub":
      walkExpr(e.lhs, out);
      walkExpr(e.rhs, out);
      break;
    case "mul":
      e.factors?.forEach((f) => walkExpr(f, out));
      break;
    case "div":
      walkExpr(e.num, out);
      walkExpr(e.den, out);
      break;
    case "min":
    case "max":
      e.args?.forEach((a) => walkExpr(a, out));
      break;
    case "clamp":
      walkExpr(e.x, out);
      walkExpr(e.lo, out);
      walkExpr(e.hi, out);
      break;
    default:
      break;
  }
}
