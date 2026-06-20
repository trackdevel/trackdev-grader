# How grades are computed

This document explains, in plain terms and with formulas, how a student's
final grade is produced — and **why each part of the formula exists and what
it accounts for**. It also gives a careful proof that **the size of the team
does not, by itself, raise or lower a student's grade**.

The grading engine is the pure-Rust crate `grade_core`. It is a *function* of
two inputs:

1. the **raw data** in `grading.db` (tasks, story points, AI declarations,
   pull requests, repository metrics, flags), and
2. the **grading spec** `config/grading.standard.json` (weights, AI maps, and
   the formulas as an expression tree).

Nothing is hand-edited into the database; re-running the engine on the same
inputs always yields the same grades. The formulas quoted below are the ones
in `config/grading.standard.json`.

---

## 1. The three levels

Grading happens in three stages, each feeding the next:

```
TASK level     →   how much of each task's points "survive" AI use   (keep)
PROJECT level  →   one team-wide grade for the work and its quality   (project_final)
STUDENT level  →   each member's share of that grade, minus penalties (student_final)
```

---

## 2. Task level — the AI discount (`keep`)

Each task carries **story points** (the team's own *estimate* of its size) and,
from sprint 3 on, a **declared AI usage**: which model was used (`model_m`,
0 = none … 1 = a fully autonomous frontier model) and at what level of reliance
(`level_l`, 0 = none … 1 = "the AI did essentially all of it").

```
keep = declared · (1 − (1 − floor_keep) · ai_strength · model_m · level_l)
       + (1 − declared) · undeclared_keep
```

with `floor_keep = 0.2`, `ai_strength = 1.0`, `undeclared_keep = 0.5`.

**What it accounts for.** `keep` is the fraction of a task's points that count
as the student's *own* effective work:

- **No AI** (`model_m·level_l = 0`) → `keep = 1`: you keep 100% of your points.
- **Maximum declared AI** (`model_m = level_l = 1`) → `keep = 0.2`: you keep
  only the floor, because the AI did most of the work.
- **Undeclared** (AI not declared on the task or its parent user story, and not
  in an AI-exempt early sprint) → `keep = 0.5`: a fixed middle penalty, so that
  *not declaring* is never better than declaring honestly.
- **AI-exempt** (sprints 1–2, where AI was not allowed/relevant) → treated as
  no-AI → `keep = 1`.

**Effective points** of a task = `raw_points × keep`. A student's effective
points `eᵢ` are the sum over their tasks; their raw points `rawᵢ` are the sum
of the estimates.

> Rationale: the estimate (`raw_points`) measures *ambition / scope*; `keep`
> discounts it down to *personal contribution* once a machine did part of the
> job. The two are kept separate on purpose — see the report columns "Punts
> originals" vs "Punts efectius".

---

## 3. Project level — one grade for the team's work

Three team-wide quantities are combined.

### 3a. `work_base` — how much was built

```
work_base = blend(size, complexity) · work_scale
```

`size` and `complexity` are 0–10 scores derived from **repository structural
metrics** (endpoints, entities, fragments, view-models, custom queries,
statement volume, …), normalised against the whole cohort. `work_scale` is a
frozen calibration constant so the strongest cohort team reads near 10.

> **`work_base` is not capped at 10.** The size/complexity blend is clamped to
> `[0, 10]`, but it is then multiplied by `work_scale` (= 1.2728 in the
> standard spec), so the effective ceiling is `10 · work_scale ≈ 12.73`.
> `work_scale` is calibrated so the *current* strongest cohort team lands near
> 10; a stronger future cohort could push `work_base` — and therefore
> `project_final` — above 10. The only hard cap in the whole pipeline is the
> final `clamp(…, 0, 10)` on `student_final`.

> Accounts for: the *amount and sophistication of the product actually
> delivered*. It is measured from the repositories, not from story points, so a
> team cannot inflate it by over-estimating tasks.

### 3b. `quality_multiplier` — how well it was built

```
quality_multiplier = quality_floor + quality_blend · quality_eff / 10
                   = 0.5 + 0.5 · quality_eff / 10
```

`quality_eff` is the 0–10 **quality axis** — **maintainability index plus
mutation score when present, and nothing else**. It is the number shown as
**"Composite quality"** in the reports.

> Architecture, complexity, and static-analysis breaches are **not** in this
> axis. They are charged once through the 80/20 code-quality penalty (§4d):
> 80% to the author who wrote them, 20% to the team via `project_final`.
> Keeping the axis to maintainability + mutation avoids the old double-count
> (where `layer_dependency` hit both the axis and the per-student hotspot).

> Accounts for: *code quality as a modulator of the work grade*. With the
> standard weights it ranges over `[0.5, 1.0]`: even poor quality keeps half
> the work grade (`quality_floor`), and top quality keeps all of it. Quality
> scales the work; it is deliberately not an additive bonus.

### 3c. `ai_factor` — how much the team leaned on AI

```
ai_factor = sum_eff / sum_raw
```

where `sum_raw = Σ rawᵢ` (all estimated points in the team) and
`sum_eff = Σ eᵢ` (all effective points after the per-task `keep`). Because every
`keep ∈ (0, 1]`, **`ai_factor ∈ (0, 1]`**: it is 1 when the team used no AI and
drops toward the floor the more the team relied on AI.

> Accounts for: *the team-wide AI discount*. It is exactly "effective points ÷
> estimated points" for the whole team.

### 3d. The project grade

```
project_final = max(work_base · quality_multiplier · ai_factor − team_quality_penalty, 0)
```

> A team that built a lot (`work_base`), built it well (`quality_multiplier`),
> and did so with little AI (`ai_factor ≈ 1`) scores high. More AI use pulls
> the *whole* project grade down through `ai_factor`. `team_quality_penalty` is
> the collective 20% share of the team's code-quality breaches (§4d). The
> result is the number that is then split among the students.

For brevity below, write the **pre-AI work-quality grade** as

```
Q = work_base · quality_multiplier        (so project_final = Q · ai_factor − team_quality_penalty)
```

`Q` depends only on the repositories and code quality — **not** on the number
of students, and **not** on story-point totals.

---

## 4. Student level — each member's share

```
student_contribution = eᵢ / sum_eff
student_base         = project_final · student_contribution · team_size
student_penalty      = penalty_on · min(student_penalty_cap, crit_flag_points · critical_flags)
student_net          = student_base − student_penalty − codequality_penalty
student_final        = clamp(student_net + lift(student_net), 0, 10)            (lift: §4e)
```

(`codequality_penalty` here is the student's **80% author share** of their own
code-quality breaches — see §4d. `lift` is the bottom-raising leniency curve —
see §4e.)

### 4a. `student_contribution` — share of the team's effective work

`contribution = eᵢ / sum_eff` is the student's slice of the team's total
effective work. The slices of all members sum to 1.

> Accounts for: *who did the work*. Crucially the denominator is `sum_eff`
> (effective, not raw), so a member who leaned on AI has a smaller `eᵢ` and a
> smaller slice.

### 4b. `× team_size` — the per-capita normaliser (this is the key term)

A "share that sums to 1" would shrink as the team grows (a member of a
6-person team has a smaller share than the same member in a 3-person team).
Multiplying by `team_size` converts the share into a **multiple of the average
member**, which lives on the same 0–10 scale as the project grade. That is the
*only* reason `team_size` appears — to undo the `1/team_size` shrinkage of the
share. (See the proof in §5; it cancels exactly.)

### 4c. Penalties (per-student, absolute)

- `student_penalty`: subtracts for *behavioural* CRITICAL flags (e.g. approving
  a PR that does not compile; `LOW_REVIEWS` — too few PR reviews given relative
  to the team, since review effort is collaboration not captured by effective
  points). `crit_flag_points = 0.75` per flag, capped at `student_penalty_cap =
  1.0`. Flags in `grade_core::policy::BEHAVIOURAL_FLAGS_UNGRADED` are excluded by
  policy — currently `ZERO_TASKS` and `LOW_COMPOSITE_SCORE` (still detected and
  reported, but not penalised: both re-charge contribution already captured by
  effective points; the *review* dimension is recovered as the separate
  `LOW_REVIEWS` flag).
- `codequality_penalty`: the student's **80% author share** of their own
  architecture / complexity / static-analysis breaches (§4d), capped at
  `qpen_author_cap`, detailed on the "Qualitat del codi" sheet.

> Both are *absolute point deductions for that individual*. They do not depend
> on team size and are not shared across the team.

`student_final` is clamped to `[0, 10]`.

### 4d. Code-quality penalty — 80% author / 20% team

Every code-quality **finding** (architecture incl. `layer_dependency`, cyclomatic
complexity, static analysis) is charged **once**, split 80/20.

1. **Blame attribution.** Each finding is attributed to the student(s) who wrote
   the offending lines (`git blame`, statement-level).
2. **Dampening + severity.** Findings are grouped by `(rule, file)` (for
   complexity, `(file, method)`); the in-group blame is capped at 1.0, so a
   single systemic pattern firing many times in one file counts **once**. Each
   group is severity-weighted (CRITICAL = 1.0, WARNING = 0.25, INFO = 0.1). This
   stops one repeated mistake (e.g. a banned API used 100×) from dominating.
3. **Points.** Per student, per signal `s`: `pts_s = min(qpen_sig_cap, scale_s ·
   blameᵢ,ₛ)`; the student's quality points `Pᵢ = Σ_s pts_s`.
4. **Split.**
   - **Author (80%):** `codequality_penalty = min(qpen_author_cap, 0.8 · Pᵢ)`,
     subtracted from that student's grade.
   - **Team (20%):** `team_quality_penalty = min(qpen_team_cap, 0.2 · Σᵢ Pᵢ)`,
     subtracted from `project_final` (so it lowers the project grade and is shared
     by everyone — collective ownership of code review is a course premise).

> Why team-wide and not the reviewer? Only one (effectively arbitrary) reviewer
> per PR was enforced, and the whole team owns every PR. AI is individual credit;
> code quality is a collective deliverable.

> All `qpen_*` weights live in `config/grading.standard.json` and are tunable in
> the desktop. The quality axis (§3b) carries no architecture term, so a finding
> is never charged twice.

`student_final` is clamped to `[0, 10]`.

### 4e. Leniency curve — lift the bottom, keep the top

A smooth curve raises low final grades while leaving good grades essentially
untouched. With `s = student_net`:

```
lift(s) = student_lift_k · s · max(0, student_lift_pivot − s) / student_lift_pivot
student_final = clamp(s + lift(s), 0, 10)
```

Defaults: `student_lift_pivot = 7`, `student_lift_k = 0.64`.

> Shape: the lift is **zero at `s = 0`** (a no-effort student isn't rewarded),
> **zero at and above the pivot 7** (good grades are kept), and bulges smoothly
> in between — most for the mid-low band. E.g. a net of 3.7 → ≈4.8, 5 → ≈5.4,
> 6 → ≈6.1, **7 → 7.0, 8 → 8.0** unchanged.

> This lives at the **student** level on purpose. A student's low grade is often
> their *own* penalty (e.g. a behavioural CRITICAL flag), not their project — so
> lifting it via the project grade would over-inflate good teams and even invert
> rankings. The student curve targets the individual without that side effect.

> `student_lift_k` sets the strength (0 = off, higher = more lift);
> `student_lift_pivot` sets where the lift stops. There is also an *optional*
> project-level curve (`project_grade_gamma`, default 1.0 = off) if you ever
> want to shape project grades instead.

---

## 5. Team size does not influence a student's grade

This is the property we want to be sure of. We show it two ways.

### 5a. Simplify the formula — `team_size` cancels

Start from the student base grade and substitute the definitions:

```
student_base = project_final · contribution · team_size
             = (Q · ai_factor) · (eᵢ / sum_eff) · team_size
             = Q · (sum_eff / sum_raw) · (eᵢ / sum_eff) · team_size      ← ai_factor = sum_eff/sum_raw
             = Q · eᵢ / sum_raw · team_size                              ← sum_eff cancels
             = Q · eᵢ / (sum_raw / team_size)
             = Q · eᵢ / mean_raw                                         ← mean_raw = sum_raw / team_size
```

and since `eᵢ = rawᵢ · keepᵢ`:

```
┌─────────────────────────────────────────────────────────┐
│  student_base = Q · keepᵢ · ( rawᵢ / mean_raw )           │
└─────────────────────────────────────────────────────────┘

   Q        = work_base · quality_multiplier   (project work-quality, team-level)
   keepᵢ    = the student's OWN AI retention    (1 if they used no AI)
   rawᵢ     = the student's OWN estimated points
   mean_raw = sum_raw / team_size = AVERAGE estimated points per member
```

The literal head-count `team_size` has **disappeared**. What remains is
`rawᵢ / mean_raw` — the student's estimated points **relative to the team
average**. Adding or removing members changes a grade *only* if it changes that
average; the count itself is gone.

(The collective `team_quality_penalty` term was dropped above for clarity. It
rides on `project_final` too, so it contributes
`−team_quality_penalty · eᵢ/mean_eff` — again per-capita, with `team_size`
cancelling the same way. So the full grade stays team-size-invariant. The §4e
leniency curve is a function of `student_net` alone — itself team-size-invariant
— so it preserves the property too.)

### 5b. Why that means "team size doesn't matter"

- **An average member always scores the project grade.** If a student does
  exactly the average effective work, `eᵢ = mean_eff = sum_eff/team_size`, then
  `contribution = 1/team_size` and

  ```
  student_base = project_final · (1/team_size) · team_size = project_final
  ```

  — the same whether the team has 3, 5, or 8 people.

- **Scaling the team changes nothing.** Imagine cloning a 4-person team into an
  8-person team where each new member mirrors an existing one. Then `sum_raw`
  and `sum_eff` both double, `team_size` doubles, but `mean_raw` is unchanged,
  `Q` is unchanged (it comes from the repos and code quality), and `ai_factor =
  sum_eff/sum_raw` is unchanged. So every `student_base = Q · keepᵢ ·
  rawᵢ/mean_raw` is **identical**. The head-count had no effect.

- **What *does* matter is relative effort, not the count.** A teammate who does
  little lowers `mean_raw`, nudging everyone else slightly *up* (they now stand
  above a lower average); a superstar nudges everyone else slightly *down*.
  This is the contribution mechanism working as intended — it is driven by the
  *average*, never by the number of people.

> In short: `team_size` is present purely as a scale normaliser and cancels
> algebraically. A student's grade is set by **their own estimated points
> relative to the team average, their own AI usage, and the team's
> work-quality grade** — not by how many people are on the team.

---

## 6. AI is charged to the individual, not the team

The same simplification shows the AI fairness property:

```
student_base = Q · keepᵢ · (rawᵢ / mean_raw)
```

`keepᵢ` is the student's **own** retention factor. **No teammate's `keep`
appears.** Consequences:

- A student who used **no AI** has `keepᵢ = 1` and scores
  `Q · rawᵢ/mean_raw` — exactly what they'd score in a team where nobody used
  AI. They are **not** penalised for their teammates' AI use.
- A student who leaned on AI keeps less (`keepᵢ < 1`) and absorbs that
  reduction themselves.
- The team total `Σ student_base = Q · ai_factor · team_size` does drop with
  more AI — but the drop falls entirely on the members who used the AI.

---

## 7. Worked example — `pds26-2a`

Real numbers from the current database (standard spec):

```
work_base = 9.90        quality_multiplier = 0.939     ⇒  Q = 9.30
quality (axis)  = 8.78  ("Composite quality" — modulates Q, not the same as Q)
sum_raw = 1688          sum_eff = 1158.9               ⇒  ai_factor = 0.687
project_final = 9.30 · 0.687 = 6.38
team_size = 4           mean_raw = 1688 / 4 = 422
```

Per student, `student_base = Q · keepᵢ · rawᵢ / mean_raw = 9.30 · keepᵢ · rawᵢ / 422`:

| student        | rawᵢ | keepᵢ | eᵢ = raw·keep | base = 9.30·keep·raw/422 | penalties | final |
|----------------|-----:|------:|--------------:|-------------------------:|----------:|------:|
| Sureda, Óscar  |  458 | 0.626 |         286.6 |  9.30·286.6/1688·4 = 6.31| −0.50 cq  | 5.81  |
| Vilà Antonescu |  430 | 0.769 |         330.5 |                     7.28 | —         | 7.28  |
| (member 3)     |  407 | 0.769 |         313.8 |                     6.91 | −1.00 beh | 5.91  |
| (member 4)     |  393 | 0.580 |         228.0 |                     5.02 | −1.75     | 3.27  |

Counterfactual: had Vilà Antonescu used **no AI** (`keep = 1`), his base would
be `9.30 · 1 · 430/422 = 9.49`, regardless of the team having 4 members or 6.
His grade tracks **his own** AI usage and **his own** estimated share — not the
team's size.

---

## 8. Variable glossary

| Symbol | Name | Meaning | Depends on team size? |
|---|---|---|---|
| `rawᵢ` | raw / original points | student's estimated story points | no (own) |
| `keepᵢ` | keep / Factor IA | fraction kept after the student's own AI use | no (own) |
| `eᵢ` | effective points | `rawᵢ · keepᵢ` | no (own) |
| `sum_raw`, `sum_eff` | team totals | Σ over the team | scales *with* the team |
| `mean_raw` | average estimate | `sum_raw / team_size` | **no** (per-capita) |
| `ai_factor` | team AI factor | `sum_eff / sum_raw ∈ (0,1]` | no (a ratio) |
| `work_base` | work delivered | size & complexity from the repos | no |
| `quality_multiplier` | quality modulator | `0.5 + 0.5 · quality_eff/10 ∈ [0.5,1]` | no |
| `Q` | work-quality grade | `work_base · quality_multiplier` | no |
| `project_final` | project grade | `Q · ai_factor` | no |
| `contribution` | effective share | `eᵢ / sum_eff` (sums to 1) | shrinks as `1/team_size` |
| `team_size` | head-count | enrolled members; **normaliser only** | cancels (see §5) |
| `student_base` | base grade | `Q · keepᵢ · rawᵢ/mean_raw` | **no** |
| `student_final` | final grade | `clamp(base − penalties, 0, 10)` | **no** |

---

## 9. Where this lives in the code

| Piece | File |
|---|---|
| `keep`, effective points, `sum_raw/sum_eff`, `ai_factor`, `contribution` | `crates/grade_core/src/shape.rs` |
| `work_base`, `quality_multiplier`, axis scores | `crates/grade_core/src/axes.rs` |
| formula evaluation (task → project → student) | `crates/grade_core/src/grade.rs` |
| the formulas & weights themselves | `config/grading.standard.json` |
| student-facing workbook | `crates/grade_xlsx/src/lib.rs` |

To recompute grades after changing the spec, reload the desktop app or run
`sprint-grader grade-explain` / `grade-xlsx` (grading is a pure function of
`grading.db` + the spec; no re-collection needed).
