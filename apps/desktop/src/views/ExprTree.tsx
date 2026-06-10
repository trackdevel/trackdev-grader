import { exprToInfix, type Expr } from "../config/infix";

/**
 * Structural tree view of a formula AST (no values — those are per-project
 * and shown on the detail pages). Mirrors the look of the evaluated Tree.
 */

const OP_LABELS: Record<Expr["op"], string> = {
  num: "number",
  var: "variable",
  add: "+ sum",
  sub: "− subtract",
  mul: "× product",
  div: "÷ divide",
  min: "min",
  max: "max",
  clamp: "clamp",
};

function childrenOf(expr: Expr): Array<{ role: string | null; node: Expr }> {
  switch (expr.op) {
    case "num":
    case "var":
      return [];
    case "add":
      return expr.terms.map((node) => ({ role: null, node }));
    case "sub":
      return [
        { role: "from", node: expr.lhs },
        { role: "minus", node: expr.rhs },
      ];
    case "mul":
      return expr.factors.map((node) => ({ role: null, node }));
    case "div":
      return [
        { role: "numerator", node: expr.num },
        { role: "denominator", node: expr.den },
      ];
    case "min":
    case "max":
      return expr.args.map((node) => ({ role: null, node }));
    case "clamp":
      return [
        { role: "x", node: expr.x },
        { role: "lo", node: expr.lo },
        { role: "hi", node: expr.hi },
      ];
  }
}

function NodeLabel({ expr, role }: { expr: Expr; role: string | null }) {
  const leafText =
    expr.op === "num" ? String(expr.value) : expr.op === "var" ? expr.name : null;
  return (
    <span className="tree-label">
      {role && <span className="tree-role">{role}: </span>}
      <strong>{leafText ?? OP_LABELS[expr.op]}</strong>
      {leafText === null && <span className="tree-expr"> {exprToInfix(expr)}</span>}
    </span>
  );
}

export default function ExprTree({
  expr,
  role = null,
  depth = 0,
}: {
  expr: Expr;
  role?: string | null;
  depth?: number;
}) {
  const children = childrenOf(expr);

  if (children.length === 0) {
    return (
      <div className="tree-leaf" style={{ paddingLeft: `${depth}rem` }}>
        <NodeLabel expr={expr} role={role} />
      </div>
    );
  }

  return (
    <details className="tree-node" open={depth < 2}>
      <summary>
        <NodeLabel expr={expr} role={role} />
      </summary>
      <div className="tree-children">
        {children.map((child, i) => (
          <ExprTree key={i} expr={child.node} role={child.role} depth={depth + 1} />
        ))}
      </div>
    </details>
  );
}
