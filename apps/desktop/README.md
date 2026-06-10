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

Click **Open grading.db** (in the header) and choose a `.db` file. The app loads all projects in the file and recomputes grades immediately using the current grading spec.

On launch, if the process working directory contains **`grader.desktop.json`**, the app loads that session file automatically (database and grading-spec paths inside it are resolved relative to the config file's folder).

Session toolbar (header):

| Action | Effect |
|--------|--------|
| **Save configuration** | Writes `grader.desktop.json` in the cwd, or overwrites the config file you loaded |
| **Save configuration as…** | Pick a path for a new session file |
| **Load configuration…** | Open a `grader.desktop.json` (or compatible JSON) from disk |
| **Reload grader.desktop.json** | Re-read the cwd session file without a file dialog |

Example `grader.desktop.json` (paths relative to the config file):

```json
{
  "version": 1,
  "grading_db": "data/entregues/grading.db",
  "grading_spec": "config/grading.custom.json"
}
```

### 2. The three tabs

The main tab bar has three options:

- **Students** — sortable table of every student across all loaded teams (final grade, base grade, penalties, AI keep, contribution, review gate). Click a student for the full detail page: grade breakdown, formula trees with evaluated values, per-task scores, flags, and AI-detection summaries.
- **Projects** — project list with final grade. Click a project for everything else: quality axes, formula tree, per-student summary, critical findings, flags.
- **Formula** — the grading formula itself (see below).

URLs use hash routing, for example:

- `#/students` and `#/students/<project_id>/<student_id>`
- `#/projects` and `#/projects/<project_id>`
- `#/formula`

(legacy `#/student/…` / `#/project/…` links still resolve.)

### 3. Formula tab

Shows the actual formula evaluated by the engine, organised by scope (task / project / student), each rendered as an expandable structure tree. Click **Edit** on any formula to change it as plain infix text — operators `+ - * /`, functions `min`, `max`, `clamp` — and **Apply**. Edits are parsed, validated (unknown variables, forward references), and trigger a live recomputation (~350 ms debounce).

Also on this tab:

| Section | What you can change |
|---------|---------------------|
| **Advanced JSON** | Add, remove, or rename formulas by editing the formulas object directly |
| **Parameters** | Penalty mode, decimals, axis weights, AI model/level multiplier maps |
| **Custom fields** | Per-project manual inputs (see below) |

Toolbar actions:

- **Open spec…** — load a custom `*.json` grading spec from disk
- **Save spec…** — write the current spec (to the open file, or pick a new path)
- **Reset to bundled default** — restore `config/grading.standard.json` from the repo

### 4. Parity banner

When using the unmodified bundled spec, the banner confirms grades match the reference baseline. After editing weights or formulas, it switches to a “tuned” state so you know results differ from the shipped standard.

### 5. Custom fields

Some grade components are entered by hand rather than computed from the database — for example an oral-defense grade or a peer-evaluation adjustment. The **Custom fields** section of the Formula tab lets you:

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
