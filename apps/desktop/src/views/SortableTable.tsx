import { useMemo, useState, type ReactNode } from "react";

type SortDir = "asc" | "desc";

type Column<T> = {
  key: string;
  header: string;
  sortable?: boolean;
  numeric?: boolean;
  render: (row: T) => ReactNode;
  sortValue?: (row: T) => string | number | null;
};

type Props<T> = {
  columns: Column<T>[];
  rows: T[];
  rowKey: (row: T) => string;
  defaultSort?: { key: string; dir: SortDir };
  caption?: string;
};

export default function SortableTable<T>({
  columns,
  rows,
  rowKey,
  defaultSort,
  caption,
}: Props<T>) {
  const [sort, setSort] = useState(defaultSort ?? { key: columns[0]?.key ?? "", dir: "asc" as SortDir });

  const sorted = useMemo(() => {
    const col = columns.find((c) => c.key === sort.key);
    if (!col?.sortable) return rows;
    const dir = sort.dir === "asc" ? 1 : -1;
    return [...rows].sort((a, b) => {
      const av = col.sortValue ? col.sortValue(a) : null;
      const bv = col.sortValue ? col.sortValue(b) : null;
      if (av == null && bv == null) return 0;
      if (av == null) return 1;
      if (bv == null) return -1;
      if (typeof av === "number" && typeof bv === "number") return (av - bv) * dir;
      return String(av).localeCompare(String(bv)) * dir;
    });
  }, [columns, rows, sort.dir, sort.key]);

  const toggleSort = (key: string) => {
    setSort((prev) =>
      prev.key === key
        ? { key, dir: prev.dir === "asc" ? "desc" : "asc" }
        : { key, dir: "asc" },
    );
  };

  return (
    <table className="sortable-table">
      {caption ? <caption>{caption}</caption> : null}
      <thead>
        <tr>
          {columns.map((col) => (
            <th
              key={col.key}
              className={col.sortable ? "sortable" : undefined}
              onClick={col.sortable ? () => toggleSort(col.key) : undefined}
            >
              {col.header}
              {col.sortable && sort.key === col.key ? (sort.dir === "asc" ? " ▲" : " ▼") : null}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {sorted.map((row) => (
          <tr key={rowKey(row)}>
            {columns.map((col) => (
              <td key={col.key}>{col.render(row)}</td>
            ))}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export function fmtNum(v: number | null | undefined, decimals = 2): string {
  if (v === null || v === undefined) return "";
  return v.toFixed(decimals);
}
