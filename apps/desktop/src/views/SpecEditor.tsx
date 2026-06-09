import { useCallback, useState } from "react";
import { writeTextFile } from "@tauri-apps/plugin-fs";

import {
  loadBundledDefault,
  openSpecFile,
  parseSpecJson,
  saveSpecAs,
  specToJson,
} from "../config/load";
import type { GradeSpec } from "../data/types";

type Props = {
  spec: GradeSpec;
  validationError: string | null;
  edited: boolean;
  specPath: string | null;
  onChange: (spec: GradeSpec) => void;
  onReset: () => void;
  onSpecPath: (path: string | null) => void;
};

export default function SpecEditor({
  spec,
  validationError,
  edited,
  specPath,
  onChange,
  onReset,
  onSpecPath,
}: Props) {
  const [formulasText, setFormulasText] = useState(() => JSON.stringify(spec.formulas, null, 2));
  const [formulasError, setFormulasError] = useState<string | null>(null);

  const updateWeight = useCallback(
    (key: string, value: string) => {
      const n = Number(value);
      if (Number.isNaN(n)) return;
      onChange({
        ...spec,
        weights: { ...spec.weights, [key]: n },
      });
    },
    [onChange, spec],
  );

  const updateMap = useCallback(
    (mapKey: "models" | "levels", key: string, value: string) => {
      const n = Number(value);
      if (Number.isNaN(n)) return;
      onChange({
        ...spec,
        [mapKey]: { ...spec[mapKey], [key]: n },
      });
    },
    [onChange, spec],
  );

  const updateMeta = useCallback(
    (field: "penalty_mode" | "decimals", value: string) => {
      if (field === "decimals") {
        const n = Number(value);
        if (Number.isNaN(n)) return;
        onChange({ ...spec, meta: { ...spec.meta, decimals: n } });
      } else {
        onChange({ ...spec, meta: { ...spec.meta, penalty_mode: value } });
      }
    },
    [onChange, spec],
  );

  const applyFormulas = useCallback(() => {
    try {
      const formulas = JSON.parse(formulasText) as GradeSpec["formulas"];
      const next = { ...spec, formulas };
      parseSpecJson(JSON.stringify(next));
      setFormulasError(null);
      onChange(next);
    } catch (e) {
      setFormulasError(e instanceof Error ? e.message : String(e));
    }
  }, [formulasText, onChange, spec]);

  const handleOpen = useCallback(async () => {
    try {
      const opened = await openSpecFile();
      if (opened) {
        onChange(opened.spec);
        onSpecPath(opened.path);
        setFormulasText(JSON.stringify(opened.spec.formulas, null, 2));
      }
    } catch (e) {
      setFormulasError(e instanceof Error ? e.message : String(e));
    }
  }, [onChange, onSpecPath]);

  const handleSave = useCallback(async () => {
    try {
      if (specPath) {
        await writeTextFile(specPath, specToJson(spec));
      } else {
        const path = await saveSpecAs(spec);
        if (path) onSpecPath(path);
      }
    } catch (e) {
      setFormulasError(e instanceof Error ? e.message : String(e));
    }
  }, [onSpecPath, spec, specPath]);

  const handleReset = useCallback(() => {
    const d = loadBundledDefault();
    onReset();
    setFormulasText(JSON.stringify(d.formulas, null, 2));
    setFormulasError(null);
  }, [onReset]);

  const weightEntries = Object.entries(spec.weights).sort(([a], [b]) => a.localeCompare(b));
  const modelEntries = Object.entries(spec.models).sort(([a], [b]) => a.localeCompare(b));
  const levelEntries = Object.entries(spec.levels).sort(([a], [b]) => a.localeCompare(b));

  return (
    <section className="spec-editor">
      <div className="spec-toolbar">
        <button type="button" onClick={() => void handleOpen()}>
          Open spec…
        </button>
        <button type="button" onClick={() => void handleSave()}>
          Save spec…
        </button>
        <button type="button" onClick={handleReset}>
          Reset to bundled default
        </button>
        {edited && <span className="badge edited">● edited</span>}
        {!edited && <span className="badge standard">✓ standard</span>}
      </div>

      {(validationError || formulasError) && (
        <p className="error">{validationError ?? formulasError}</p>
      )}

      <div className="spec-grid">
        <fieldset>
          <legend>Meta</legend>
          <label>
            Penalty mode
            <select
              value={spec.meta.penalty_mode}
              onChange={(e) => updateMeta("penalty_mode", e.target.value)}
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
              onChange={(e) => updateMeta("decimals", e.target.value)}
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

      <fieldset>
        <legend>Formulas (JSON)</legend>
        <textarea
          className="formulas-editor"
          value={formulasText}
          onChange={(e) => setFormulasText(e.target.value)}
          spellCheck={false}
        />
        <button type="button" onClick={applyFormulas}>
          Apply formulas JSON
        </button>
      </fieldset>
    </section>
  );
}
