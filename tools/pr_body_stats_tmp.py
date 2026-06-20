#!/usr/bin/env python3
"""One-off PR body length stats for grading.db."""
import sqlite3
import statistics
from pathlib import Path

DB = Path(__file__).resolve().parents[1] / "data/grading.db"


def id_snippet(s: str, n: int = 16) -> str:
    s = str(s)
    return s if len(s) <= n else s[:n] + "..."


def percentiles(vals: list[int], ps: list[float]) -> dict[float, float | None]:
    if not vals:
        return {p: None for p in ps}
    s = sorted(vals)
    n = len(s)
    out: dict[float, float | None] = {}
    for p in ps:
        if n == 1:
            out[p] = float(s[0])
            continue
        k = (n - 1) * (p / 100.0)
        f = int(k)
        c = min(f + 1, n - 1)
        out[p] = float(s[f]) if f == c else float(s[f] + (k - f) * (s[c] - s[f]))
    return out


def load_rows(cur, sql: str) -> list[dict]:
    cur.execute(sql)
    out = []
    for r in cur.fetchall():
        body = r["body"]
        char_len = 0 if body is None else len(body)
        byte_len = 0 if body is None else r["byte_len"]
        out.append(
            {
                "id": r["id"],
                "pr_number": r["pr_number"],
                "repo_full_name": r["repo_full_name"],
                "body": body,
                "byte_len": byte_len,
                "char_len": char_len,
            }
        )
    return out


def report(label: str, total: int, sql_max_byte: int | None, data: list[dict]) -> None:
    max_char = max((d["char_len"] for d in data), default=0)
    top3 = sorted(data, key=lambda d: d["char_len"], reverse=True)[:3]
    nonempty = [d["char_len"] for d in data if d["body"] is not None and len(d["body"]) > 0]
    med = statistics.median(nonempty) if nonempty else None
    pct = percentiles(nonempty, [95, 99])
    over = sum(1 for d in data if d["char_len"] > 3000)
    over_pct = 100.0 * over / total if total else 0.0

    print(f"=== {label} ===")
    print(f"total_count={total}")
    print(f"max_byte_len_sqlite={sql_max_byte}")
    print(f"max_char_len_python={max_char}")
    print("top3:")
    for i, d in enumerate(top3, 1):
        print(
            f"  {i}. id_snippet={id_snippet(d['id'])} "
            f"pr_number={d['pr_number']} repo={d['repo_full_name']} "
            f"byte_len={d['byte_len']} char_len={d['char_len']}"
        )
    print(f"nonempty_count={len(nonempty)}")
    print(f"median_char={med}")
    print(f"p95_char={pct[95]}")
    print(f"p99_char={pct[99]}")
    print(f"over_3000_count={over}")
    print(f"over_3000_pct={over_pct:.6f}")


def main() -> None:
    conn = sqlite3.connect(DB)
    conn.row_factory = sqlite3.Row
    cur = conn.cursor()

    cur.execute("SELECT COUNT(*) FROM pull_requests")
    total = cur.fetchone()[0]
    cur.execute("SELECT MAX(length(body)) FROM pull_requests")
    sql_max_byte = cur.fetchone()[0]
    data = load_rows(
        cur,
        "SELECT id, pr_number, repo_full_name, body, length(body) AS byte_len FROM pull_requests",
    )
    report("ALL pull_requests", total, sql_max_byte, data)

    grad_sql = """
        SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name, pr.body,
               length(pr.body) AS byte_len
        FROM pull_requests pr
        INNER JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
        INNER JOIN tasks t ON t.id = tpr.task_id
        WHERE t.type != 'USER_STORY'
    """
    grad_data = load_rows(cur, grad_sql)
    cur.execute(
        """
        SELECT MAX(length(pr.body))
        FROM pull_requests pr
        INNER JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
        INNER JOIN tasks t ON t.id = tpr.task_id
        WHERE t.type != 'USER_STORY'
        """
    )
    grad_max_byte = cur.fetchone()[0]
    report("GRADABLE (task type != USER_STORY)", len(grad_data), grad_max_byte, grad_data)
    conn.close()


if __name__ == "__main__":
    main()
