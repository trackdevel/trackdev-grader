# `train_regressor` — ridge head trainer for the local-hybrid PR doc evaluator

This script fits three ridge-regression models — one per axis
(`title_score`, `description_score`, `total_doc_score`) — and writes them
to `data/regressor/pr_{title,description,total}.json`. The Rust pipeline
(`crates/evaluate_local`) loads those JSONs via
`PrRidgeBundle::load_optional` whenever `[evaluate] judge = "local-hybrid"`.

The script is **operator-run**, not part of CI. It needs a labelled
grading.db (rows in `pr_doc_evaluation`) and a running ollama daemon with
the BGE-M3 embedding model pulled.

## Prerequisites

1. **A populated grading.db** — at least `--min-train` (default 20) rows
   in `pr_doc_evaluation` with non-null `title_score`, `description_score`,
   `total_doc_score`. Run `sprint-grader run-all` once with the Haiku
   judge on the first sprint to seed labels:

   ```bash
   # In config/course.toml: [evaluate] judge = "claude-cli"
   sprint-grader run-all --today YYYY-MM-DD
   ```

2. **ollama** with **BGE-M3** pulled. From a host with GPU access:

   ```bash
   curl -fsSL https://ollama.com/install.sh | sh
   ollama pull bge-m3
   ollama serve   # leave running; defaults to 127.0.0.1:11434
   ```

3. **Python ≥ 3.10**. Install deps in a venv:

   ```bash
   python3 -m venv .venv
   . .venv/bin/activate
   pip install -r tools/train_regressor/requirements.txt
   ```

## Train

```bash
python tools/train_regressor/train.py \
  --db data/entregues/grading.db \
  --ollama http://127.0.0.1:11434 \
  --embed-model bge-m3 \
  --out data/regressor
```

The script:

1. Reads every labelled row out of `pr_doc_evaluation` joined with
   `pull_requests` + `tasks`.
2. Builds an embedding input per PR using a format **byte-identical** to
   `crates/evaluate_local/src/pipeline.rs::build_pr_embedding_input` —
   the Rust integration test
   `embed_input_matches_trainer_shape` is the determinism gate.
3. POSTs each batch of 32 inputs to `<ollama>/api/embed`.
4. Stratifies an 80/20 train/test split by rounded `total_doc_score`
   bucket.
5. Fits a `sklearn.linear_model.Ridge(alpha=1.0)` per axis and persists
   the coefficients + intercept + residual stddev as JSON.
6. Writes Spearman + quarter-grid agreement to `data/regressor/metrics.json`.

Exit code is non-zero with a clear stderr message when fewer than
`--min-train` labelled rows are available. The minimum can be lowered
via `--min-train N` but you'll get noisy predictions.

## Cold start — first term, no labels yet

The local-hybrid evaluator can't learn from nothing. The bootstrap
procedure for a new course (or a fresh database):

1. Pin `[evaluate] judge = "claude-cli"` in `config/course.toml`.
2. Run `sprint-grader run-all` once for the first sprint. The CLI judge
   produces ~150 labelled rows per sprint at the cost of ~150 Claude
   subscription calls.
3. Flip `[evaluate] judge = "local-hybrid"`.
4. Run this trainer to produce `data/regressor/`.
5. Every subsequent `sprint-grader run-all` will use the local path —
   approximately zero Claude subscription cost in steady state.

## Retraining

Whenever the grading rubric changes (i.e. labels drift) or you add a new
sprint's worth of labels:

1. Run the trainer again. It overwrites `data/regressor/pr_*.json`.
2. Run `sprint-grader reset-local-scores` to invalidate previously
   persisted `pr_doc_evaluation` rows produced by the local judge
   (justification prefix `local:`). Non-local rows are preserved.
3. Re-run `sprint-grader run-all` to repopulate.

## Reviewing the result

`data/regressor/metrics.json` carries `spearman` + `quarter_grid_agreement`
per axis. Rule-of-thumb thresholds:

| metric | acceptable | suspect |
|---|---|---|
| `total.spearman` | ≥ 0.50 | < 0.30 |
| `total.quarter_grid_agreement` | ≥ 0.40 | < 0.25 |

If `total.spearman < 0.50`, widen the borderline band in
`[evaluate.local]` so more PRs hit the LLM:

```toml
[evaluate.local]
pr_total_band_low  = 0.0
pr_total_band_high = 6.0
```

The regressor becomes advisory; every borderline PR still gets the LLM's
opinion in P3. Treat regressor metrics as informational, not hard targets.

## Calibration vs. the Anthropic-judged baseline

A temporary side-by-side calibration workflow is supported — leave the
judge on `"claude-cli"` for a chosen subset of projects, run the trainer
on the remainder, and `diff-db` the two grading.db snapshots. Do **not**
introduce a new judge value to support side-by-side; flip
`[evaluate] judge` between runs instead.

## Why ollama and not direct GPU?

Invariant O from `PLAN_LOCAL_SCORING_v3.md`: the GPU stack stays a black
box behind the ollama daemon. The Rust process never links `ort`,
`mistralrs`, `nvidia-smi`, or any other GPU artefact. The same constraint
holds here — every model call is an HTTP request — so the trainer
can run on a different host from ollama if that host has the GPU.

If your training box can't reach ollama directly, expose the daemon over
a tunnel (e.g. `ssh -L 11434:127.0.0.1:11434`) and point `--ollama` at
`http://127.0.0.1:11434`. Do not introduce a fallback embedding pathway
in this script — the operator decision is documented in the plan's
"Operator Decisions" section.
