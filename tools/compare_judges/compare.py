#!/usr/bin/env python3
"""Side-by-side comparison of Claude (Haiku) and Salamandra-2B on a
stratified PR sample.

Pulls a diverse handful of real PRs from `grading.db`, scores each one
through both judges using the exact same prompt formats production uses,
and emits a markdown report so the operator can eyeball where the two
models disagree.

Prompt parity:
  - Claude path mirrors `crates/evaluate/src/llm_eval.rs::evaluate_prs_via_cli`
    (user message includes `PR #N in <repo>` line and a trailing
    "Return ONLY the JSON object" reminder).
  - Salamandra path mirrors
    `crates/evaluate_local/src/pipeline.rs::build_pr_user_message`
    (Task / User Story / Title / Description, no PR # line, schema-constrained
    sampling via ollama's `format` field). Defaults `temperature=0`, `top_k=1`,
    `seed=0` to match the Rust production call.

The script is read-only against the DB — it never writes to
pr_doc_evaluation or any other table."""

from __future__ import annotations

import argparse
import json
import random
import re
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

import requests

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
DEFAULT_RUBRIC = REPO_ROOT / "crates" / "evaluate" / "assets" / "prompts" / "rubric_pr_compact.md"

# Schema posted to ollama as `format` to constrain Salamandra's reply.
# Mirrors `pr_response_schema` in pipeline.rs — keep in sync.
PR_RESPONSE_SCHEMA = {
    "type": "object",
    "required": ["title_score", "description_score"],
    "properties": {
        "title_score": {"type": "number", "minimum": 0.0, "maximum": 2.0},
        "description_score": {"type": "number", "minimum": 0.0, "maximum": 4.0},
        "total_doc_score": {"type": "number", "minimum": 0.0, "maximum": 6.0},
        "justification": {"type": "string"},
    },
}

# Reused JSON-block extractor — same heuristic as `extract_json_object`
# in crates/evaluate/src/llm_eval.rs so loose/fenced replies parse.
_JSON_FENCE = re.compile(r"```(?:json)?\s*({.*?})\s*```", re.DOTALL)
_JSON_BARE = re.compile(r"({.*})", re.DOTALL)


def extract_json_object(text: str):
    """Return the first JSON object found in `text`, or None.

    Accepts: bare JSON, ```json fenced JSON, JSON wrapped in prose."""
    if not text:
        return None
    for pattern in (_JSON_FENCE, _JSON_BARE):
        match = pattern.search(text)
        if not match:
            continue
        try:
            return json.loads(match.group(1))
        except json.JSONDecodeError:
            continue
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return None


# ───────────────────────────── Candidate selection ──────────────────────────


def fetch_candidates(db_path: Path):
    """Return one row per (PR, task) pair, joined with averaged Claude
    scores from pr_doc_evaluation. Used as the sampling pool."""
    conn = sqlite3.connect(db_path)
    cur = conn.cursor()
    cur.execute(
        """
        SELECT p.id          AS pr_id,
               p.pr_number   AS pr_number,
               p.repo_full_name AS repo,
               p.title       AS title,
               p.body        AS body,
               t.name        AS task_name,
               parent.name   AS parent_story,
               proj.name     AS project_name,
               AVG(pd.title_score)       AS avg_title,
               AVG(pd.description_score) AS avg_desc,
               AVG(pd.total_doc_score)   AS avg_total,
               COUNT(pd.pr_id)           AS n_labels
        FROM pull_requests p
        JOIN task_pull_requests tpr ON tpr.pr_id = p.id
        JOIN tasks t                 ON t.id = tpr.task_id
        LEFT JOIN tasks parent       ON parent.id = t.parent_task_id
        JOIN sprints s               ON s.id = t.sprint_id
        JOIN projects proj           ON proj.id = s.project_id
        LEFT JOIN pr_doc_evaluation pd ON pd.pr_id = p.id
        WHERE t.type != 'USER_STORY'
        GROUP BY p.id
        """
    )
    rows = [
        dict(
            pr_id=r[0],
            pr_number=r[1],
            repo=r[2] or "",
            title=r[3] or "",
            body=r[4] or "",
            task_name=r[5] or "",
            parent_story=r[6] or "N/A",
            project_name=r[7] or "",
            avg_title=r[8],
            avg_desc=r[9],
            avg_total=r[10],
            n_labels=r[11] or 0,
        )
        for r in cur.fetchall()
    ]
    conn.close()
    return rows


def length_tier(body: str) -> str:
    n = len(body.strip())
    if n == 0:
        return "empty"
    if n < 100:
        return "tiny"
    if n < 400:
        return "short"
    if n < 1500:
        return "medium"
    return "long"


def stratified_sample(rows, n_total: int, seed: int):
    """Pick `n_total` PRs spread across the five length tiers and a
    smattering of titles. Buckets undersampled if a tier has fewer PRs
    than the per-bucket quota."""
    rng = random.Random(seed)
    by_tier: dict[str, list] = {}
    for row in rows:
        by_tier.setdefault(length_tier(row["body"]), []).append(row)
    tiers = ["empty", "tiny", "short", "medium", "long"]
    per = max(1, n_total // len(tiers))
    out = []
    for tier in tiers:
        pool = by_tier.get(tier, [])
        rng.shuffle(pool)
        out.extend(pool[:per])
    # Fill any shortfall by random sampling from PRs not already picked.
    if len(out) < n_total:
        seen = {r["pr_id"] for r in out}
        leftover = [r for r in rows if r["pr_id"] not in seen]
        rng.shuffle(leftover)
        out.extend(leftover[: n_total - len(out)])
    return out[:n_total]


# ───────────────────────────────── Prompts ───────────────────────────────────


def build_claude_user_message(row) -> str:
    """Mirror llm_eval.rs::evaluate_prs_via_cli's user message."""
    pr_num = row["pr_number"] if row["pr_number"] is not None else ""
    body = row["body"] if row["body"] else "(empty)"
    return (
        f"Task: {row['task_name']}\n"
        f"User Story: {row['parent_story']}\n"
        f"PR #{pr_num} in {row['repo']}\n"
        f"Title: {row['title']}\n"
        f"Description:\n{body}\n\n"
        "Return ONLY the JSON object specified by the rubric — no prose, no fences."
    )


def build_ollama_user_message(row) -> str:
    """Mirror pipeline.rs::build_pr_user_message (= build_pr_embedding_input)."""
    body = row["body"] if row["body"] else "(empty)"
    return (
        f"Task: {row['task_name']}\n"
        f"User Story: {row['parent_story']}\n"
        f"Title: {row['title']}\n"
        f"Description:\n{body}"
    )


# ─────────────────────────────── Backends ────────────────────────────────────


def call_claude(cli_path: str, model: str, rubric: str, user_msg: str, timeout: int):
    """Spawn one `claude --print ...` subprocess; mirrors ClaudeCliClient."""
    argv = [
        cli_path,
        "--print",
        "--output-format",
        "text",
        "--model",
        model,
        "--append-system-prompt",
        rubric,
        "--allowedTools",
        "",
    ]
    t0 = time.monotonic()
    try:
        proc = subprocess.run(
            argv,
            input=user_msg,
            text=True,
            capture_output=True,
            timeout=timeout,
        )
    except FileNotFoundError:
        return {"error": f"claude CLI not found at `{cli_path}`", "elapsed_s": 0.0}
    except subprocess.TimeoutExpired:
        return {"error": f"claude CLI timed out after {timeout}s", "elapsed_s": float(timeout)}
    elapsed = time.monotonic() - t0
    if proc.returncode != 0:
        return {
            "error": f"claude CLI exit {proc.returncode}: {proc.stderr.strip()[:300]}",
            "elapsed_s": elapsed,
        }
    return {"raw": proc.stdout, "elapsed_s": elapsed}


def call_salamandra(
    ollama_url: str,
    model: str,
    rubric: str,
    user_msg: str,
    timeout: int,
    use_schema: bool = True,
):
    """POST to /api/chat. Mirrors OllamaClient::chat_json including
    temperature=0, top_k=1, seed=0 for reproducibility."""
    body = {
        "model": model,
        "messages": [
            {"role": "system", "content": rubric},
            {"role": "user", "content": user_msg},
        ],
        "stream": False,
        "keep_alive": "5m",
        "options": {"temperature": 0, "top_k": 1, "seed": 0},
    }
    if use_schema:
        body["format"] = PR_RESPONSE_SCHEMA
    url = ollama_url.rstrip("/") + "/api/chat"
    t0 = time.monotonic()
    try:
        resp = requests.post(url, json=body, timeout=timeout)
    except requests.RequestException as e:
        return {"error": f"ollama HTTP error: {e}", "elapsed_s": time.monotonic() - t0}
    elapsed = time.monotonic() - t0
    if resp.status_code == 400 and "format" in resp.text and use_schema:
        # Older ollama: retry without schema, mirroring the Rust fallback.
        return call_salamandra(ollama_url, model, rubric, user_msg, timeout, use_schema=False)
    if not resp.ok:
        return {"error": f"ollama status {resp.status_code}: {resp.text[:300]}", "elapsed_s": elapsed}
    try:
        content = resp.json()["message"]["content"]
    except (KeyError, ValueError, TypeError) as e:
        return {"error": f"ollama shape: {e}", "elapsed_s": elapsed}
    return {"raw": content, "elapsed_s": elapsed}


def normalize_response(raw: str | None):
    """Pull title/description/total/justification from a raw model reply."""
    if not raw:
        return {"title_score": None, "description_score": None, "total_doc_score": None, "justification": "", "parse_ok": False}
    obj = extract_json_object(raw)
    if not isinstance(obj, dict):
        return {"title_score": None, "description_score": None, "total_doc_score": None, "justification": raw.strip()[:400], "parse_ok": False}
    title = obj.get("title_score")
    desc = obj.get("description_score")
    total = obj.get("total_doc_score")
    if total is None and title is not None and desc is not None:
        total = float(title) + float(desc)
    return {
        "title_score": float(title) if isinstance(title, (int, float)) else None,
        "description_score": float(desc) if isinstance(desc, (int, float)) else None,
        "total_doc_score": float(total) if isinstance(total, (int, float)) else None,
        "justification": (obj.get("justification") or "").strip(),
        "parse_ok": title is not None and desc is not None,
    }


# ───────────────────────────────── Render ────────────────────────────────────


def fmt_score(x):
    if x is None:
        return "—"
    return f"{x:.2f}"


def render_markdown(results, args) -> str:
    lines = []
    lines.append(f"# Judge comparison ({len(results)} PRs)\n")
    lines.append(
        f"- Claude: `{args.claude_model}` via `{args.claude_cli}` (timeout {args.claude_timeout}s)\n"
        f"- Salamandra: `{args.ollama_model}` via `{args.ollama}` (timeout {args.ollama_timeout}s)\n"
        f"- Seed: {args.seed}\n\n"
    )

    # Summary table.
    lines.append("## Summary\n\n")
    lines.append(
        "| # | Project | PR | Body len | Tier | Claude title/desc/total | Salamandra title/desc/total | DB avg (n) |\n"
        "|--:|---|---|--:|---|---|---|---|\n"
    )
    for i, r in enumerate(results, 1):
        row = r["row"]
        cl = r["claude_norm"]
        ol = r["ollama_norm"]
        db_avg = (
            f"{row['avg_title']:.2f}/{row['avg_desc']:.2f}/{row['avg_total']:.2f} ({row['n_labels']})"
            if row["avg_total"] is not None
            else "—"
        )
        lines.append(
            f"| {i} | {row['project_name']} | "
            f"[#{row['pr_number']}](https://github.com/{row['repo']}/pull/{row['pr_number']}) | "
            f"{len(row['body'])} | {length_tier(row['body'])} | "
            f"{fmt_score(cl['title_score'])}/{fmt_score(cl['description_score'])}/{fmt_score(cl['total_doc_score'])} | "
            f"{fmt_score(ol['title_score'])}/{fmt_score(ol['description_score'])}/{fmt_score(ol['total_doc_score'])} | "
            f"{db_avg} |\n"
        )
    lines.append("\n")

    # Per-PR detail.
    lines.append("## Per-PR detail\n\n")
    for i, r in enumerate(results, 1):
        row = r["row"]
        cl = r["claude_norm"]
        ol = r["ollama_norm"]
        body_preview = row["body"][:600] + ("…" if len(row["body"]) > 600 else "")
        lines.append(f"### {i}. {row['project_name']} — PR #{row['pr_number']}\n\n")
        lines.append(
            f"- **Repo:** `{row['repo']}`\n"
            f"- **Task:** {row['task_name']}\n"
            f"- **User Story:** {row['parent_story']}\n"
            f"- **Title:** {row['title']!r}\n"
            f"- **Body length:** {len(row['body'])} chars ({length_tier(row['body'])} tier)\n"
            f"- **DB avg (Claude, n={row['n_labels']}):** "
        )
        if row["avg_total"] is not None:
            lines.append(
                f"title {row['avg_title']:.2f}, desc {row['avg_desc']:.2f}, total {row['avg_total']:.2f}\n\n"
            )
        else:
            lines.append("none\n\n")

        if body_preview.strip():
            lines.append("**Body:**\n\n```\n" + body_preview + "\n```\n\n")
        else:
            lines.append("**Body:** _(empty)_\n\n")

        lines.append("**Claude:**\n\n")
        if "error" in r["claude"]:
            lines.append(f"_error:_ {r['claude']['error']}\n\n")
        else:
            lines.append(
                f"- title {fmt_score(cl['title_score'])}, "
                f"desc {fmt_score(cl['description_score'])}, "
                f"total {fmt_score(cl['total_doc_score'])}  "
                f"({r['claude']['elapsed_s']:.1f}s, parse_ok={cl['parse_ok']})\n"
            )
            if cl["justification"]:
                lines.append(f"- justification: {cl['justification']}\n")
            lines.append("\n")

        lines.append("**Salamandra:**\n\n")
        if "error" in r["ollama"]:
            lines.append(f"_error:_ {r['ollama']['error']}\n\n")
        else:
            lines.append(
                f"- title {fmt_score(ol['title_score'])}, "
                f"desc {fmt_score(ol['description_score'])}, "
                f"total {fmt_score(ol['total_doc_score'])}  "
                f"({r['ollama']['elapsed_s']:.1f}s, parse_ok={ol['parse_ok']})\n"
            )
            if ol["justification"]:
                lines.append(f"- justification: {ol['justification']}\n")
            lines.append("\n")

        # Delta line — quick eye-catcher.
        if cl["parse_ok"] and ol["parse_ok"]:
            d_title = ol["title_score"] - cl["title_score"]
            d_desc = ol["description_score"] - cl["description_score"]
            d_total = ol["total_doc_score"] - cl["total_doc_score"]
            lines.append(
                f"**Δ (Salamandra − Claude):** title {d_title:+.2f}, "
                f"desc {d_desc:+.2f}, total {d_total:+.2f}\n\n"
            )
        lines.append("---\n\n")
    return "".join(lines)


# ───────────────────────────────── Main ──────────────────────────────────────


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--db", type=Path, default=REPO_ROOT / "data" / "entregues" / "grading.db")
    p.add_argument("--rubric", type=Path, default=DEFAULT_RUBRIC)
    p.add_argument("--n", type=int, default=15, help="number of PRs to compare (default 15)")
    p.add_argument("--seed", type=int, default=42)
    p.add_argument("--claude-cli", default="claude", help="path to claude binary")
    p.add_argument("--claude-model", default="claude-haiku-4-5-20251001")
    p.add_argument("--claude-timeout", type=int, default=180)
    p.add_argument("--ollama", default="http://127.0.0.1:11434")
    p.add_argument(
        "--ollama-model",
        default="hf.co/BSC-LT/salamandra-2b-instruct-GGUF:Q5_K_M",
    )
    p.add_argument("--ollama-timeout", type=int, default=120)
    p.add_argument(
        "--out",
        type=Path,
        default=REPO_ROOT / "tools" / "compare_judges" / "comparison.md",
    )
    p.add_argument(
        "--no-claude",
        action="store_true",
        help="skip Claude calls (useful for testing the Salamandra side only)",
    )
    p.add_argument(
        "--no-ollama",
        action="store_true",
        help="skip Salamandra calls",
    )
    return p.parse_args()


def main() -> int:
    args = parse_args()
    if not args.db.exists():
        sys.stderr.write(f"DB not found: {args.db}\n")
        return 1
    if not args.rubric.exists():
        sys.stderr.write(f"rubric not found: {args.rubric}\n")
        return 1

    rubric = args.rubric.read_text(encoding="utf-8")
    candidates = fetch_candidates(args.db)
    sys.stderr.write(f"loaded {len(candidates)} candidate PRs from {args.db}\n")

    sample = stratified_sample(candidates, args.n, args.seed)
    sys.stderr.write(f"selected {len(sample)} PRs across tiers: ")
    sys.stderr.write(
        ", ".join(
            f"{t}={sum(1 for r in sample if length_tier(r['body']) == t)}"
            for t in ("empty", "tiny", "short", "medium", "long")
        )
        + "\n"
    )

    results = []
    for i, row in enumerate(sample, 1):
        sys.stderr.write(
            f"  [{i}/{len(sample)}] {row['project_name']} PR #{row['pr_number']} ({length_tier(row['body'])})… "
        )
        sys.stderr.flush()

        if args.no_claude:
            claude_result = {"error": "skipped (--no-claude)", "elapsed_s": 0.0}
        else:
            claude_result = call_claude(
                args.claude_cli,
                args.claude_model,
                rubric,
                build_claude_user_message(row),
                args.claude_timeout,
            )

        if args.no_ollama:
            ollama_result = {"error": "skipped (--no-ollama)", "elapsed_s": 0.0}
        else:
            ollama_result = call_salamandra(
                args.ollama,
                args.ollama_model,
                rubric,
                build_ollama_user_message(row),
                args.ollama_timeout,
            )

        claude_norm = normalize_response(claude_result.get("raw"))
        ollama_norm = normalize_response(ollama_result.get("raw"))

        sys.stderr.write(
            f"Cl={fmt_score(claude_norm['total_doc_score'])} "
            f"Sa={fmt_score(ollama_norm['total_doc_score'])}\n"
        )
        results.append(
            {
                "row": row,
                "claude": claude_result,
                "ollama": ollama_result,
                "claude_norm": claude_norm,
                "ollama_norm": ollama_norm,
            }
        )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(render_markdown(results, args), encoding="utf-8")
    sys.stderr.write(f"\nwrote {args.out}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
