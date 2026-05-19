#!/usr/bin/env python3
"""Train ridge regressors for sprint-grader's local-hybrid PR doc evaluator.

The Rust side (`crates/evaluate_local/src/ridge.rs`) consumes three
sidecar JSONs — `pr_{title,description,total}.json` — produced here.
Embeddings are fetched from a running ollama daemon over HTTP; this
script never imports a GPU stack directly (Invariant O).
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import sys
import time
from pathlib import Path

import numpy as np
import requests
from scipy.stats import spearmanr
from sklearn.linear_model import RidgeCV
from sklearn.model_selection import GroupShuffleSplit, train_test_split

# Mirror of `crates/evaluate_local/src/pipeline.rs::EMBED_BATCH`. Keep in
# sync; throughput-only knob — does not affect determinism.
EMBED_BATCH = 32
MIN_TRAIN_DEFAULT = 20

# Log-spaced α grid for RidgeCV's leave-one-out GCV. Spans 3.5 orders of
# magnitude around α=1, dense enough to land on a well-calibrated value
# at any plausible (samples, features) ratio without burning compute.
DEFAULT_ALPHAS = (0.01, 0.03, 0.1, 0.3, 1.0, 3.0, 10.0, 30.0)

import sqlite3


def fetch_pr_rows(db_path: Path):
    """Return labelled PR rows from `pr_doc_evaluation` joined with the
    PR metadata the embedding format depends on.

    Schema parity with the Rust side: SELECT order matches the columns
    `build_inputs` expects. Skips USER_STORY-typed parents (they are not
    PR-bearing tasks)."""
    conn = sqlite3.connect(db_path)
    cur = conn.cursor()
    cur.execute(
        """
        SELECT pd.pr_id,
               pd.sprint_id,
               p.title,
               p.body,
               t.name AS task_name,
               t2.name AS parent_story,
               pd.title_score,
               pd.description_score,
               pd.total_doc_score
        FROM pr_doc_evaluation pd
        JOIN pull_requests p ON p.id = pd.pr_id
        JOIN task_pull_requests tpr ON tpr.pr_id = pd.pr_id
        JOIN tasks t ON t.id = tpr.task_id
        LEFT JOIN tasks t2 ON t2.id = t.parent_task_id
        WHERE t.type != 'USER_STORY'
          AND pd.title_score IS NOT NULL
          AND pd.description_score IS NOT NULL
          AND pd.total_doc_score IS NOT NULL
        """
    )
    rows = cur.fetchall()
    conn.close()
    return rows


def build_inputs(rows):
    """Byte-identical to `pipeline.rs::build_pr_embedding_input`.

    If you change the format here, change it there too — and rerun the
    `embed_input_matches_trainer_shape` test. Drift between the two is
    silent: predictions degrade but nothing throws."""
    inputs = []
    for _pr_id, _sprint, title, body, task_name, parent_story, *_labels in rows:
        task = task_name if task_name is not None else ""
        story = parent_story if parent_story is not None else "N/A"
        t = title if title is not None else ""
        b = body if body is not None else "(empty)"
        inputs.append(f"Task: {task}\nUser Story: {story}\nTitle: {t}\nDescription:\n{b}")
    return inputs


def dedupe_by_pr(rows):
    """Collapse multi-row PRs into a single representative row with
    averaged labels.

    The trainer's SQL joins through task_pull_requests, so a PR linked
    to N tasks yields N rows — each row has its OWN Claude scoring,
    independent of the others. In practice Claude at temp=0 has ~0.4
    point intra-PR spread (diagnostic on the current corpus: 54 % of
    PRs are multi-labelled; avg spread is 0.40; 9 PRs have ≥2.0
    spread). Ridge fits every row as independent ground truth, so the
    residual stddev floor is bounded by this label noise.

    Deduping here breaks training-inference parity in a minor way:
    production still emits one pr_doc_evaluation row per (PR, task),
    but those rows have near-identical embedder inputs (task_name +
    parent_story prefixes are short relative to the rubric content),
    so they get near-identical predictions. The resulting per-PR
    score is more stable, not less.

    Representative row choice is deterministic: lowest sprint_id, then
    alphabetical task_name. Labels are simple averages across the
    group."""
    groups: dict[str, list] = {}
    for row in rows:
        groups.setdefault(row[0], []).append(row)
    deduped = []
    for pr_id, group in groups.items():
        rep = sorted(group, key=lambda r: (r[1], r[4] or ""))[0]
        n = len(group)
        title_avg = sum(r[6] for r in group) / n
        desc_avg = sum(r[7] for r in group) / n
        total_avg = sum(r[8] for r in group) / n
        deduped.append(
            (rep[0], rep[1], rep[2], rep[3], rep[4], rep[5], title_avg, desc_avg, total_avg)
        )
    deduped.sort(key=lambda r: r[0])
    return deduped


def embed_batch(ollama_url: str, model: str, inputs: list[str]) -> list[list[float]]:
    """POST /api/embed with three-attempt exponential backoff."""
    url = ollama_url.rstrip("/") + "/api/embed"
    body = {"model": model, "input": inputs}
    last_err = None
    for attempt in range(3):
        try:
            r = requests.post(url, json=body, timeout=120)
            r.raise_for_status()
            data = r.json()
            if "embeddings" in data:
                return data["embeddings"]
            if "data" in data:
                return [item["embedding"] for item in data["data"]]
            raise ValueError(f"unexpected embed response shape: keys={list(data.keys())}")
        except Exception as e:  # noqa: BLE001
            last_err = e
            if attempt < 2:
                time.sleep(2 ** attempt)
    raise RuntimeError(f"ollama embed failed after 3 attempts: {last_err}")


def embed_all(ollama_url: str, model: str, inputs: list[str]) -> np.ndarray:
    vectors = []
    for i in range(0, len(inputs), EMBED_BATCH):
        chunk = inputs[i : i + EMBED_BATCH]
        vectors.extend(embed_batch(ollama_url, model, chunk))
    arr = np.asarray(vectors, dtype=np.float64)
    if arr.ndim != 2:
        raise ValueError(f"embedding matrix has rank {arr.ndim}; expected 2")
    return arr


def fit_and_save(
    X: np.ndarray,
    y: np.ndarray,
    out_dir: Path,
    name: str,
    embed_model: str,
    dim: int,
) -> dict:
    """Fit a RidgeCV head and persist it as `pr_<name>.json` in the
    on-disk shape `RidgeHead::load` expects.

    Uses leave-one-out GCV across DEFAULT_ALPHAS to pick the
    regularisation strength. The chosen α is written to the payload as
    `alpha` for auditability; the Rust loader ignores unknown fields."""
    model = RidgeCV(alphas=DEFAULT_ALPHAS).fit(X, y)
    coefficients = model.coef_.astype(float).tolist()
    intercept = float(model.intercept_)
    chosen_alpha = float(model.alpha_)
    residuals = y - model.predict(X)
    residual_stddev = float(np.std(residuals, ddof=1)) if len(residuals) > 1 else 0.0
    payload = {
        "embedding_model": embed_model,
        "embedding_dim": dim,
        "intercept": intercept,
        "coefficients": coefficients,
        "residual_stddev": residual_stddev,
        "alpha": chosen_alpha,
        "n_train": int(X.shape[0]),
        "trained_at": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
    }
    out_path = out_dir / f"pr_{name}.json"
    with out_path.open("w", encoding="utf-8") as f:
        json.dump(payload, f, indent=2)
    return payload


def quantize_to_quarter(values: np.ndarray) -> np.ndarray:
    return np.round(values * 4.0) / 4.0


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Train ridge heads for the local-hybrid PR doc evaluator.")
    p.add_argument("--db", type=Path, required=True, help="Path to grading.db")
    p.add_argument(
        "--ollama",
        default="http://127.0.0.1:11434",
        help="ollama base URL (default %(default)s)",
    )
    p.add_argument(
        "--embed-model",
        default="bge-m3",
        help="ollama embedding model tag (default %(default)s)",
    )
    p.add_argument(
        "--out",
        type=Path,
        default=Path("data/regressor"),
        help="output directory for ridge JSONs",
    )
    p.add_argument(
        "--min-train",
        type=int,
        default=MIN_TRAIN_DEFAULT,
        help=f"minimum labelled rows required (default {MIN_TRAIN_DEFAULT})",
    )
    p.add_argument(
        "--test-split",
        type=float,
        default=0.2,
        help="held-out fraction (default 0.20)",
    )
    p.add_argument(
        "--no-dedupe",
        action="store_true",
        help="keep one row per (PR, task) pair (legacy behaviour; "
        "produces a noisier training set — see dedupe_by_pr docstring).",
    )
    return p.parse_args()


def main() -> int:
    args = parse_args()
    if not args.db.exists():
        sys.stderr.write(f"DB not found: {args.db}\n")
        return 1

    rows = fetch_pr_rows(args.db)
    if len(rows) < args.min_train:
        sys.stderr.write(
            f"Only {len(rows)} labelled rows in {args.db}; need at least {args.min_train}.\n"
            f"Run `claude-cli`-judge once on the first sprint to seed labels, "
            f"or lower --min-train if you know what you're doing.\n"
        )
        return 1

    raw_unique = len({row[0] for row in rows})
    sys.stderr.write(
        f"  {len(rows)} raw rows from {raw_unique} unique PRs "
        f"({len(rows) / max(raw_unique, 1):.1f} rows/PR)\n"
    )
    if not args.no_dedupe:
        rows = dedupe_by_pr(rows)
        sys.stderr.write(
            f"  deduped to {len(rows)} rows (one per PR, labels averaged across fan-out)\n"
        )

    inputs = build_inputs(rows)
    labels = np.asarray(
        [(row[-3], row[-2], row[-1]) for row in rows], dtype=np.float64
    )
    pr_ids = np.asarray([row[0] for row in rows])
    n_unique_prs = int(len(np.unique(pr_ids)))

    sys.stderr.write(f"embedding {len(inputs)} PRs via {args.embed_model} on {args.ollama}…\n")
    X = embed_all(args.ollama, args.embed_model, inputs)
    sys.stderr.write(f"  shape = {X.shape}\n")

    # Group-aware split: every row for a given pr_id lands in train OR
    # test, never both. Stratification is dropped (GroupShuffleSplit can't
    # stratify and group simultaneously); the label distribution is
    # close-enough to balanced at this corpus size that random group
    # sampling is fine.
    if n_unique_prs >= 2:
        splitter = GroupShuffleSplit(
            n_splits=1, test_size=args.test_split, random_state=0
        )
        idx_train, idx_test = next(splitter.split(np.arange(len(rows)), groups=pr_ids))
    else:
        idx_train, idx_test = train_test_split(
            np.arange(len(rows)), test_size=args.test_split, random_state=0
        )

    args.out.mkdir(parents=True, exist_ok=True)
    metrics = {}
    for axis_idx, name in enumerate(["title", "description", "total"]):
        head = fit_and_save(
            X[idx_train],
            labels[idx_train, axis_idx],
            args.out,
            name,
            args.embed_model,
            X.shape[1],
        )
        # Validation telemetry.
        preds = np.dot(X[idx_test], np.asarray(head["coefficients"])) + head["intercept"]
        spearman = float(spearmanr(preds, labels[idx_test, axis_idx]).statistic)
        # Quarter-grid agreement (closest-cell kappa proxy).
        preds_grid = quantize_to_quarter(preds)
        labels_grid = quantize_to_quarter(labels[idx_test, axis_idx])
        kappa = float(np.mean(preds_grid == labels_grid))
        metrics[name] = {
            "spearman": spearman if not math.isnan(spearman) else 0.0,
            "quarter_grid_agreement": kappa,
            "n_train": int(len(idx_train)),
            "n_test": int(len(idx_test)),
            "n_train_unique_prs": int(len(np.unique(pr_ids[idx_train]))),
            "n_test_unique_prs": int(len(np.unique(pr_ids[idx_test]))),
        }
        sys.stderr.write(
            f"  pr_{name}.json — spearman={metrics[name]['spearman']:.3f}, "
            f"grid_agreement={metrics[name]['quarter_grid_agreement']:.3f}, "
            f"alpha={head['alpha']:g}\n"
        )

    (args.out / "metrics.json").write_text(json.dumps(metrics, indent=2))
    sys.stderr.write(f"wrote {args.out}/pr_*.json + metrics.json\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
