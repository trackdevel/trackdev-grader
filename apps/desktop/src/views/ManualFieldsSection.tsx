import { useState } from "react";

import { confirm } from "@tauri-apps/plugin-dialog";

import type { ManualFields, RawProject } from "../data/types";

type Props = {
  fields: ManualFields;
  projects: RawProject[];
  onChange: (next: ManualFields) => void;
};

/** Rename a field key across a project_id → field → V map, preserving entries. */
function renameFieldKey<V>(
  map: Record<string, Record<string, V>>,
  oldName: string,
  newName: string,
): Record<string, Record<string, V>> {
  // Transiently blank name (mid-edit): leave entries under the old key rather
  // than dropping them — they're recoverable once a real name is typed.
  if (!newName) return map;
  const out: Record<string, Record<string, V>> = {};
  for (const [pid, row] of Object.entries(map)) {
    const next = { ...row };
    if (oldName in next) {
      next[newName] = next[oldName];
      delete next[oldName];
    }
    out[pid] = next;
  }
  return out;
}

/** Drop a field key from every row of a project_id → field → V map. */
function dropFieldKey<V>(
  map: Record<string, Record<string, V>>,
  name: string,
): Record<string, Record<string, V>> {
  const out: Record<string, Record<string, V>> = {};
  for (const [pid, row] of Object.entries(map)) {
    const next = { ...row };
    delete next[name];
    out[pid] = next;
  }
  return out;
}

/**
 * Custom (manual) fields: professor-entered per-project numbers that become
 * variables usable in the project and student formulas. Each per-project value
 * may carry a free-text explanation (display/audit only; never graded).
 */
export default function ManualFieldsSection({ fields: mf, projects, onChange }: Props) {
  const notes = mf.notes ?? {};

  // Which field's per-project values are shown. Index-based and clamped so the
  // selection survives add/remove/rename happening in the definitions table.
  const [activeField, setActiveField] = useState(0);
  const activeIndex = Math.min(Math.max(activeField, 0), mf.defs.length - 1);

  const addField = () => {
    const existing = new Set(mf.defs.map((d) => d.name));
    let n = mf.defs.length + 1;
    let name = `field_${n}`;
    while (existing.has(name)) {
      n += 1;
      name = `field_${n}`;
    }
    onChange({ ...mf, defs: [...mf.defs, { name, value: 0, description: "" }] });
  };

  const renameField = (index: number, newName: string) => {
    const oldName = mf.defs[index].name;
    if (newName === oldName) return;
    const defs = mf.defs.map((d, i) => (i === index ? { ...d, name: newName } : d));
    onChange({
      defs,
      values: renameFieldKey(mf.values, oldName, newName),
      notes: renameFieldKey(notes, oldName, newName),
    });
  };

  const setDefault = (index: number, raw: string) => {
    const v = Number(raw);
    if (Number.isNaN(v)) return;
    onChange({ ...mf, defs: mf.defs.map((d, i) => (i === index ? { ...d, value: v } : d)) });
  };

  const setDescription = (index: number, description: string) => {
    onChange({ ...mf, defs: mf.defs.map((d, i) => (i === index ? { ...d, description } : d)) });
  };

  const removeField = async (index: number) => {
    const def = mf.defs[index];
    const affected = Object.values(mf.values).filter(
      (row) => typeof row[def.name] === "number",
    ).length;
    if (affected > 0) {
      const ok = await confirm(
        `Delete "${def.name}"? This also clears entered values and explanations for ${affected} team(s).`,
        { title: "Delete custom field", kind: "warning" },
      );
      if (!ok) return;
    }
    const defs = mf.defs.filter((_, i) => i !== index);
    onChange({
      defs,
      values: dropFieldKey(mf.values, def.name),
      notes: dropFieldKey(notes, def.name),
    });
  };

  const setValue = (projectId: number, name: string, raw: string) => {
    const key = String(projectId);
    const values: ManualFields["values"] = { ...mf.values };
    const row = { ...(values[key] ?? {}) };
    if (raw.trim() === "") {
      delete row[name]; // blank → inherit the field default
    } else {
      const v = Number(raw);
      if (Number.isNaN(v)) return;
      row[name] = v;
    }
    values[key] = row;
    onChange({ ...mf, values });
  };

  const setNote = (projectId: number, name: string, raw: string) => {
    const key = String(projectId);
    const next: NonNullable<ManualFields["notes"]> = { ...notes };
    const row = { ...(next[key] ?? {}) };
    if (raw.trim() === "") {
      delete row[name];
    } else {
      row[name] = raw;
    }
    next[key] = row;
    onChange({ ...mf, notes: next });
  };

  return (
    <>
      <fieldset>
        <legend>Custom field definitions ({mf.defs.length})</legend>
        <p className="hint">
          Each field name becomes a variable usable in the project and student formulas; a
          blank per-project cell inherits the field default.
        </p>
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
                  No custom fields defined yet.
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
          (() => {
            const d = mf.defs[activeIndex];
            return (
              <>
                <div className="manual-tabs" role="tablist">
                  {mf.defs.map((f, i) => (
                    <button
                      key={i}
                      type="button"
                      role="tab"
                      aria-selected={i === activeIndex}
                      className={i === activeIndex ? "manual-tab active" : "manual-tab"}
                      onClick={() => setActiveField(i)}
                    >
                      {f.name}
                    </button>
                  ))}
                </div>
                {d.description ? <p className="hint">{d.description}</p> : null}
                <table className="manual-values-table">
                  <thead>
                    <tr>
                      <th>team</th>
                      <th>value</th>
                      <th>explanation</th>
                    </tr>
                  </thead>
                  <tbody>
                    {projects.map((p) => {
                      const v = mf.values[String(p.project_id)]?.[d.name];
                      const note = notes[String(p.project_id)]?.[d.name];
                      return (
                        <tr key={p.project_id}>
                          <td>
                            {p.name} <span className="hint">#{p.project_id}</span>
                          </td>
                          <td>
                            <input
                              type="number"
                              step="any"
                              value={v === undefined ? "" : v}
                              placeholder={String(d.value)}
                              onChange={(e) => setValue(p.project_id, d.name, e.target.value)}
                            />
                          </td>
                          <td>
                            <textarea
                              className="manual-note"
                              rows={2}
                              spellCheck={false}
                              value={note ?? ""}
                              placeholder="Explain the value (optional)"
                              onChange={(e) => setNote(p.project_id, d.name, e.target.value)}
                            />
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </>
            );
          })()
        )}
      </fieldset>
    </>
  );
}
