import type { ExplainNode } from "../data/types";

type Props = {
  node: ExplainNode;
  depth?: number;
};

function fmtValue(v: number): string {
  return Number.isInteger(v) ? String(v) : v.toFixed(4).replace(/\.?0+$/, "");
}

export default function Tree({ node, depth = 0 }: Props) {
  const hasChildren = node.children.length > 0;
  const summary = (
    <span className="tree-label">
      <strong>{node.label}</strong>
      {node.expr ? <span className="tree-expr"> = {node.expr}</span> : null}
      <span className="tree-value"> → {fmtValue(node.value)}</span>
    </span>
  );

  if (!hasChildren) {
    return (
      <div className="tree-leaf" style={{ paddingLeft: `${depth}rem` }}>
        {summary}
      </div>
    );
  }

  return (
    <details className="tree-node" open={depth < 2}>
      <summary>{summary}</summary>
      <div className="tree-children">
        {node.children.map((child, i) => (
          <Tree key={`${child.label}-${i}`} node={child} depth={depth + 1} />
        ))}
      </div>
    </details>
  );
}

export function FormulaTreeList({
  items,
}: {
  items: Array<{ name: string; node: ExplainNode }>;
}) {
  if (!items.length) return <p className="hint">No formula tree available.</p>;
  return (
    <div className="tree-list">
      {items.map((item) => (
        <div key={item.name} className="tree-formula-block">
          <h4>{item.name}</h4>
          <Tree node={item.node} />
        </div>
      ))}
    </div>
  );
}
