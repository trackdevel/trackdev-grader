# TrackDev Grader (desktop)

Cross-platform desktop app for browsing sprint grading data and tuning grade formulas in real time. It opens a `grading.db` file produced by the `sprint-grader` CLI, recomputes grades in the browser using the same Rust engine compiled to WebAssembly, and shows explainable breakdown trees for each student.

The app is **offline and local-first**: it only reads files you pick via the system file dialog. No network access.

## Prerequisites

Install these once on your machine:

| Tool | Purpose |
|------|---------|
| [Rust](https://rustup.rs/) (stable) | Tauri shell and WASM build |
| [Node.js](https://nodejs.org/) 20+ | Frontend tooling |
| [pnpm](https://pnpm.io/) | JavaScript package manager |
| [wasm-pack](https://rustwasm.github.io/wasm-pack/) 0.13+ | Builds the grading engine WASM bundle (recommended) |

The repo pins Rust in `rust-toolchain.toml` and includes the `wasm32-unknown-unknown` target. On first use, `rustup` will install them automatically when you run a Rust command from the repo root.

**Linux (Tauri):** install WebKit and related system libraries. On Fedora:

```bash
sudo dnf install webkit2gtk4.1-devel openssl-devel libappindicator-gtk3-devel librsvg2-devel
```

Other distributions: see [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).

**Fallback if `wasm-pack` is missing:** the build script can use `cargo build` + `wasm-bindgen-cli` instead. Install with:

```bash
cargo install wasm-bindgen-cli --version 0.2.118
```

## Install dependencies

From the repository root:

```bash
cd apps/desktop
pnpm install
pnpm run build:wasm
```

`build:wasm` compiles `grade_core` to `apps/desktop/pkg/`. Re-run it after pulling changes that touch the grading engine (`crates/grade_core` or `crates/grade_core_wasm`).

## Run the app

**Development (recommended):**

```bash
cd apps/desktop
pnpm tauri dev
```

This starts the Vite dev server on port **1420** and opens the native window. The first launch may take a minute while Rust dependencies compile.

**Frontend only** (limited — file dialogs and SQLite need Tauri):

```bash
pnpm dev
```

Opens `http://localhost:1420` in a browser. Database open and spec save/load will not work outside the Tauri shell.

## Getting a `grading.db`

The desktop app does not collect data from TrackDev or GitHub. Use the main CLI first:

```bash
# from repo root, with TRACKDEV_TOKEN and GITHUB_TOKEN set
cargo run --release -- run-all
```

The default database path is `data/entregues/grading.db`. In the app, click **Open grading.db** and select that file (or any compatible SQLite database from a grading run).

## Using the app

### 1. Open a database

Click **Open grading.db** and choose a `.db` file. The app loads all projects in the file and shows them in a summary table. Grades are recomputed immediately using the current grading spec.

### 2. Browse students and projects

Use the top navigation:

- **Students** — sortable table of every student across all loaded teams (final grade, base grade, penalties, AI keep, contribution, review gate).
- **Projects** — team-level overview with composite grades and penalties.

Click a row to open a detail page with grade breakdown, formula trees, per-task scores, flags, and AI-detection summaries.

URLs use hash routing, for example:

- `#/students`
- `#/projects`
- `#/student/<project_id>/<student_id>`
- `#/project/<project_id>`

### 3. Grading spec editor

Expand **Grading spec editor** to tune how grades are calculated. Changes debounce (~350 ms) and trigger a live recomputation.

| Section | What you can change |
|---------|---------------------|
| **Meta** | Penalty mode (`subtractive` or `off`), decimal places for displayed grades |
| **Weights** | Axis weights used in composite scoring |
| **AI models / levels** | String-to-multiplier maps for declared AI tool usage |
| **Formulas (JSON)** | Full formula AST (per-task keep, axis scores, penalties, final grade) |

Toolbar actions:

- **Open spec…** — load a custom `*.json` grading spec from disk
- **Save spec…** — write the current spec (to the open file, or pick a new path)
- **Reset to bundled default** — restore `config/grading.standard.json` from the repo

The status banner at the top shows whether you are on the bundled standard spec, have edited it, or have a validation/parity issue.

### 4. Parity banner

When using the unmodified bundled spec, the banner confirms grades match the reference baseline. After editing weights or formulas, it switches to a “tuned” state so you know results differ from the shipped standard.

### 5. Manual fields

Some grade components are entered by hand rather than computed from the database — for example an oral-defense grade or a peer-evaluation adjustment. The **Manual fields** tab (top navigation) lets you:

- **Define fields** (shared across all teams). Each field has a `name` — used directly as a variable in formulas, so it must be a valid identifier (letters, digits, underscore; not starting with a digit) and must not clash with an existing weight, scope variable, or formula name — a default `value`, and a `description`.
- **Enter per-project values** in the grid: one row per team, one column per field. A blank cell inherits the field's default.

Every field becomes a variable available in the **project** and **student** formulas, so you can fold it into the final grade, for example:

```
student_final = clamp(0.8*student_base + 0.2*oral_presentation - student_penalty, 0, 10)
```

Definitions and values are stored in the grading spec JSON under `manual_fields`, so **Save spec…** persists them and they travel with the file. **Reset to bundled default** restores the standard formulas but **keeps** your manual fields (they are data, not logic). Deleting a field that already has entered values asks for confirmation first.

## Developer commands

All commands run from `apps/desktop/`:

| Command | Description |
|---------|-------------|
| `pnpm tauri dev` | Run the desktop app in development mode |
| `pnpm dev` | Vite dev server only (port 1420) |
| `pnpm build` | Typecheck and build the frontend to `dist/` |
| `pnpm tauri build` | Production build of the Tauri app |
| `pnpm run build:wasm` | Rebuild the WASM grading engine into `pkg/` |
| `pnpm test` | Build WASM and run Vitest unit tests |
| `pnpm run lint` | ESLint on `src/` |
| `pnpm run typecheck` | TypeScript check without emit |

## Project layout (brief)

```
apps/desktop/
├── src/              React UI (views, spec editor, WASM driver)
├── src-tauri/        Tauri 2 shell (dialog, filesystem, SQLite plugins)
├── pkg/              Generated WASM bundle (run build:wasm)
└── config/           JSON schema for grading specs
```

The authoritative default grading spec lives at `config/grading.standard.json` in the repo root.

## Troubleshooting

- **“Engine: …” error after editing the spec** — invalid formula or weight; the UI keeps the last good grades. Fix the validation error shown in the spec editor.
- **Empty student/project lists** — confirm the database was produced by a recent `sprint-grader` run and contains project rows.
- **WASM build fails** — ensure `wasm-pack` is installed, or install `wasm-bindgen-cli` as described above. Run from the repo root so `crates/grade_core_wasm` resolves correctly.
- **Tauri fails on Linux** — install the WebKit/GTK development packages for your distribution (see Prerequisites).
