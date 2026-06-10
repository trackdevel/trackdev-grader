import { useState } from "react";
import { writeTextFile } from "@tauri-apps/plugin-fs";

import { parseInfix, type Expr } from "../config/infix";
import { openSpecFile, parseSpecJson, saveSpecAs, specToJson } from "../config/load";
import { validateSpec } from "../config/validate";
import type { FormulaDef, GradeSpec, ManualFields, RawProject } from "../data/types";
import ExprTree from "./ExprTree";
import ManualFieldsSection from "./ManualFieldsSection";

type FormulaScope = "task" | "project" | "student";

const SCOPE_TITLES: Array<{ scope: FormulaScope; title: string; hint: string }> = [
  { scope: "task", title: "Task formulas", hint: "evaluated once per task" },
  { scope: "project", title: "Project formulas", hint: "evaluated once per team" },
  { scope: "student", title: "Student formulas", hint: "evaluated once per student" },
];

type Props = {
  spec: GradeSpec;
  projects: RawProject[];
  validationError: string | null;
  edited: boolean;
  specPath: string | null;
  onChange: (spec: GradeSpec) => void;
  onReset: () => void;
  onSpecPath: (path: string | null) => void;
};

export default function FormulaView({
  spec,
  projects,
  validationError,
  edited,
  specPath,
  onChange,
  onReset,
  onSpecPath,
}: Props) {
  const [fileError, setFileError] = useState<string | null>(null);

  const handleOpen = async () => {
    try {
      const opened = await openSpecFile();
      if (opened) {
        onChange(opened.spec);
        onSpecPath(opened.path);
        setFileError(null);
      }
    } catch (e) {
      setFileError(e instanceof Error ? e.message : String(e));
    }
  };

  const handleSave = async () => {
    try {
      if (specPath) {
        await writeTextFile(specPath, specToJson(spec));
      } else {
        const path = await saveSpecAs(spec);
        if (path) onSpecPath(path);
      }
      setFileError(null);
    } catch (e) {
      setFileError(e instanceof Error ? e.message : String(e));
    }
  };

  /** Replace one formula; returns an error message instead of committing bad input. */
  const tryApplyFormula = (scope: FormulaScope, index: number, text: string): string | null => {
    let expr: Expr;
    try {
      expr = parseInfix(text);
    } catch (e) {
      return e instanceof Error ? e.message : String(e);
    }
    const group = spec.formulas[scope].map((f, i) =>
      i === index ? { ...f, infix: text.trim(), expr } : f,
    );
    const next = { ...spec, formulas: { ...spec.formulas, [scope]: group } };
    const result = validateSpec(next);
    if (!result.ok) return result.message;
    onChange(next);
    return null;
  };

  const setManualFields = (manual_fields: ManualFields) => {
    onChange({ ...spec, manual_fields });
  };

  return (
    <section className="view">
      <h3>Formula</h3>
      <p className="hint">
        The grading formula as evaluated by the engine. Edit any sub-formula as infix text
        (operators + − * /, functions min, max, clamp); changes recompute grades live.
      </p>

      <div className="spec-toolbar">
        <button type="button" onClick={() => void handleOpen()}>
          Open spec…
        </button>
        <button type="button" onClick={() => void handleSave()}>
          Save spec…
        </button>
        <button type="button" onClick={onReset}>
          Reset to bundled default
        </button>
        {edited ? (
          <span className="badge edited">● edited</span>
        ) : (
          <span className="badge standard">✓ standard</span>
        )}
        {specPath && <span className="meta">{specPath}</span>}
      </div>

      {(validationError || fileError) && (
        <p className="error">{validationError ?? fileError}</p>
      )}

      {SCOPE_TITLES.map(({ scope, title, hint }) => (
        <fieldset key={scope}>
          <legend>
            {title} <span className="hint">({hint})</span>
          </legend>
          {spec.formulas[scope].map((def, i) => (
            <FormulaBlock
              key={`${scope}-${def.name}`}
              def={def}
              onApply={(text) => tryApplyFormula(scope, i, text)}
            />
          ))}
        </fieldset>
      ))}

      <AdvancedJsonEditor spec={spec} onChange={onChange} />

      <ParametersSection spec={spec} onChange={onChange} />

      <h3 className="custom-fields-title">Custom fields</h3>
      <ManualFieldsSection
        fields={spec.manual_fields ?? { defs: [], values: {} }}
        projects={projects}
        onChange={setManualFields}
      />
    </section>
  );
}

/* ── one formula: name + infix + structural tree, editable in place ───── */

function FormulaBlock({
  def,
  onApply,
}: {
  def: FormulaDef;
  onApply: (text: string) => string | null;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const editing = draft !== null;

  const apply = () => {
    if (draft === null) return;
    const err = onApply(draft);
    setError(err);
    if (err === null) setDraft(null);
  };

  return (
    <div className="formula-block">
      <div className="formula-head">
        <strong>{def.name}</strong>
        {!editing && <code className="formula-infix">= {def.infix}</code>}
        {!editing ? (
          <button type="button" className="small" onClick={() => setDraft(def.infix)}>
            Edit
          </button>
        ) : (
          <>
            <button type="button" className="small" onClick={apply}>
              Apply
            </button>
            <button
              type="button"
              className="small"
              onClick={() => {
                setDraft(null);
                setError(null);
              }}
            >
              Cancel
            </button>
          </>
        )}
      </div>
      {editing && (
        <textarea
          className="formula-edit"
          value={draft}
          spellCheck={false}
          rows={2}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) apply();
          }}
        />
      )}
      {editing && error && <p className="error">{error}</p>}
      <ExprTree expr={def.expr as Expr} />
    </div>
  );
}

/* ── escape hatch: add/remove/rename formulas as raw JSON ──────────────── */

function AdvancedJsonEditor({
  spec,
  onChange,
}: {
  spec: GradeSpec;
  onChange: (spec: GradeSpec) => void;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const text = draft ?? JSON.stringify(spec.formulas, null, 2);

  const apply = () => {
    if (draft === null) return;
    try {
      const formulas = JSON.parse(draft) as GradeSpec["formulas"];
      const next = { ...spec, formulas };
      parseSpecJson(JSON.stringify(next));
      onChange(next);
      setDraft(null);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <details className="spec-panel">
      <summary>Advanced: edit formulas as JSON (add, remove, or rename formulas)</summary>
      <textarea
        className="formulas-editor"
        value={text}
        spellCheck={false}
        onChange={(e) => setDraft(e.target.value)}
      />
      <div className="spec-toolbar">
        <button type="button" onClick={apply} disabled={draft === null}>
          Apply formulas JSON
        </button>
        {draft !== null && (
          <button type="button" onClick={() => (setDraft(null), setError(null))}>
            Discard draft
          </button>
        )}
      </div>
      {error && <p className="error">{error}</p>}
    </details>
  );
}

/* ── numeric parameters referenced by the formulas ─────────────────────── */

function ParametersSection({
  spec,
  onChange,
}: {
  spec: GradeSpec;
  onChange: (spec: GradeSpec) => void;
}) {
  const updateWeight = (key: string, value: string) => {
    const n = Number(value);
    if (Number.isNaN(n)) return;
    onChange({ ...spec, weights: { ...spec.weights, [key]: n } });
  };

  const updateMap = (mapKey: "models" | "levels", key: string, value: string) => {
    const n = Number(value);
    if (Number.isNaN(n)) return;
    onChange({ ...spec, [mapKey]: { ...spec[mapKey], [key]: n } });
  };

  const weightEntries = Object.entries(spec.weights).sort(([a], [b]) => a.localeCompare(b));
  const modelEntries = Object.entries(spec.models).sort(([a], [b]) => a.localeCompare(b));
  const levelEntries = Object.entries(spec.levels).sort(([a], [b]) => a.localeCompare(b));

  return (
    <details className="spec-panel">
      <summary>Parameters: weights, AI factors, meta</summary>
      <div className="spec-grid">
        <fieldset>
          <legend>Meta</legend>
          <label>
            Penalty mode
            <select
              value={spec.meta.penalty_mode}
              onChange={(e) =>
                onChange({ ...spec, meta: { ...spec.meta, penalty_mode: e.target.value } })
              }
            >
              <option value="subtractive">subtractive</option>
              <option value="off">off</option>
            </select>
          </label>
          <label>
            Decimals
            <input
              type="number"
              min={0}
              max={6}
              value={spec.meta.decimals ?? 2}
              onChange={(e) => {
                const n = Number(e.target.value);
                if (!Number.isNaN(n)) {
                  onChange({ ...spec, meta: { ...spec.meta, decimals: n } });
                }
              }}
            />
          </label>
        </fieldset>

        <fieldset className="weights-fieldset">
          <legend>Weights ({weightEntries.length})</legend>
          <div className="weights-grid">
            {weightEntries.map(([key, val]) => (
              <label key={key}>
                {key}
                <input
                  type="number"
                  step="any"
                  value={val}
                  onChange={(e) => updateWeight(key, e.target.value)}
                />
              </label>
            ))}
          </div>
        </fieldset>

        <fieldset className="weights-fieldset">
          <legend>AI models ({modelEntries.length})</legend>
          <div className="weights-grid">
            {modelEntries.map(([key, val]) => (
              <label key={key}>
                {key}
                <input
                  type="number"
                  step="any"
                  min={0}
                  max={1}
                  value={val}
                  onChange={(e) => updateMap("models", key, e.target.value)}
                />
              </label>
            ))}
          </div>
        </fieldset>

        <fieldset className="weights-fieldset">
          <legend>AI levels ({levelEntries.length})</legend>
          <div className="weights-grid">
            {levelEntries.map(([key, val]) => (
              <label key={key}>
                {key}
                <input
                  type="number"
                  step="any"
                  min={0}
                  max={1}
                  value={val}
                  onChange={(e) => updateMap("levels", key, e.target.value)}
                />
              </label>
            ))}
          </div>
        </fieldset>
      </div>
    </details>
  );
}
