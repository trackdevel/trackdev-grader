import { useCallback } from "react";
import { confirm } from "@tauri-apps/plugin-dialog";

import type { GradeSpec, ManualFields as ManualFieldsData, RawProject } from "../data/types";

type Props = {
  spec: GradeSpec;
  projects: RawProject[];
  validationError: string | null;
  onChange: (spec: GradeSpec) => void;
};

const EMPTY: ManualFieldsData = { defs: [], values: {} };

export default function ManualFields({ spec, projects, validationError, onChange }: Props) {
  const mf = spec.manual_fields ?? EMPTY;

  const setManual = useCallback(
    (next: ManualFieldsData) => {
      onChange({ ...spec, manual_fields: next });
    },
    [onChange, spec],
  );

  const addField = useCallback(() => {
    const existing = new Set(mf.defs.map((d) => d.name));
    let n = mf.defs.length + 1;
    let name = `field_${n}`;
    while (existing.has(name)) {
      n += 1;
      name = `field_${n}`;
    }
    setManual({ defs: [...mf.defs, { name, value: 0, description: "" }], values: mf.values });
  }, [mf, setManual]);

  const renameField = useCallback(
    (index: number, newName: string) => {
      const oldName = mf.defs[index].name;
      if (newName === oldName) return;
      const defs = mf.defs.map((d, i) => (i === index ? { ...d, name: newName } : d));
      // Migrate any stored per-project values from the old key to the new one.
      // When the name is transiently blank (mid-edit), leave values under the
      // old key rather than dropping them — they're recoverable, not destroyed.
      const values: ManualFieldsData["values"] = {};
      for (const [pid, row] of Object.entries(mf.values)) {
        const next = { ...row };
        if (newName && oldName in next) {
          next[newName] = next[oldName];
          delete next[oldName];
        }
        values[pid] = next;
      }
      setManual({ defs, values });
    },
    [mf, setManual],
  );

  const setDefault = useCallback(
    (index: number, raw: string) => {
      const v = Number(raw);
      if (Number.isNaN(v)) return;
      const defs = mf.defs.map((d, i) => (i === index ? { ...d, value: v } : d));
      setManual({ defs, values: mf.values });
    },
    [mf, setManual],
  );

  const setDescription = useCallback(
    (index: number, description: string) => {
      const defs = mf.defs.map((d, i) => (i === index ? { ...d, description } : d));
      setManual({ defs, values: mf.values });
    },
    [mf, setManual],
  );

  const removeField = useCallback(
    async (index: number) => {
      const def = mf.defs[index];
      const affected = Object.values(mf.values).filter(
        (row) => typeof row[def.name] === "number",
      ).length;
      if (affected > 0) {
        const ok = await confirm(
          `Delete "${def.name}"? This also clears entered values for ${affected} team(s).`,
          { title: "Delete manual field", kind: "warning" },
        );
        if (!ok) return;
      }
      const defs = mf.defs.filter((_, i) => i !== index);
      const values: ManualFieldsData["values"] = {};
      for (const [pid, row] of Object.entries(mf.values)) {
        const next = { ...row };
        delete next[def.name];
        values[pid] = next;
      }
      setManual({ defs, values });
    },
    [mf, setManual],
  );

  const setValue = useCallback(
    (projectId: number, name: string, raw: string) => {
      const key = String(projectId);
      const values: ManualFieldsData["values"] = { ...mf.values };
      const row = { ...(values[key] ?? {}) };
      if (raw.trim() === "") {
        delete row[name]; // blank → inherit the field default
      } else {
        const v = Number(raw);
        if (Number.isNaN(v)) return;
        row[name] = v;
      }
      values[key] = row;
      setManual({ defs: mf.defs, values });
    },
    [mf, setManual],
  );

  return (
    <section className="view spec-editor">
      <h3>Manual fields</h3>
      <p className="hint">
        Per-project values entered by the professor. Each field becomes a variable usable in the
        project and student grading formulas; a blank cell inherits the field default.
      </p>

      {validationError && <p className="error">{validationError}</p>}

      <fieldset>
        <legend>Definitions ({mf.defs.length})</legend>
        <table>
          <thead>
            <tr>
              <th>name (formula variable)</th>
              <th>default</th>
              <th>description</th>
              <th aria-label="actions" />
            </tr>
          </thead>
          <tbody>
            {mf.defs.map((d, i) => (
              <tr key={i}>
                <td>
                  <input
                    type="text"
                    value={d.name}
                    spellCheck={false}
                    onChange={(e) => renameField(i, e.target.value)}
                  />
                </td>
                <td>
                  <input
                    type="number"
                    step="any"
                    value={d.value}
                    onChange={(e) => setDefault(i, e.target.value)}
                  />
                </td>
                <td>
                  <input
                    type="text"
                    value={d.description}
                    onChange={(e) => setDescription(i, e.target.value)}
                  />
                </td>
                <td>
                  <button type="button" onClick={() => void removeField(i)}>
                    Remove
                  </button>
                </td>
              </tr>
            ))}
            {mf.defs.length === 0 && (
              <tr>
                <td colSpan={4} className="hint">
                  No fields defined yet.
                </td>
              </tr>
            )}
          </tbody>
        </table>
        <button type="button" onClick={addField}>
          Add field
        </button>
      </fieldset>

      <fieldset>
        <legend>Per-project values</legend>
        {mf.defs.length === 0 ? (
          <p className="hint">Define a field above to start entering per-project values.</p>
        ) : projects.length === 0 ? (
          <p className="hint">Open a grading.db to enter per-project values.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>team</th>
                {mf.defs.map((d) => (
                  <th key={d.name} title={d.description}>
                    {d.name}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {projects.map((p) => {
                const row = mf.values[String(p.project_id)] ?? {};
                return (
                  <tr key={p.project_id}>
                    <td>
                      {p.name} <span className="hint">#{p.project_id}</span>
                    </td>
                    {mf.defs.map((d) => {
                      const v = row[d.name];
                      return (
                        <td key={d.name}>
                          <input
                            type="number"
                            step="any"
                            value={v === undefined ? "" : v}
                            placeholder={String(d.value)}
                            onChange={(e) => setValue(p.project_id, d.name, e.target.value)}
                          />
                        </td>
                      );
                    })}
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </fieldset>
    </section>
  );
}
