import { confirm } from "@tauri-apps/plugin-dialog";

import type { ConstantDef } from "../data/types";

type Props = {
  constants: ConstantDef[];
  onChange: (next: ConstantDef[]) => void;
};

/**
 * Global constants: named numbers that become variables usable in any formula
 * (task, project, and student). Unlike custom fields they have a single value,
 * not per-project overrides.
 */
export default function ConstantsSection({ constants, onChange }: Props) {
  const addConstant = () => {
    const existing = new Set(constants.map((c) => c.name));
    let n = constants.length + 1;
    let name = `const_${n}`;
    while (existing.has(name)) {
      n += 1;
      name = `const_${n}`;
    }
    onChange([...constants, { name, value: 0, description: "" }]);
  };

  const rename = (index: number, name: string) => {
    onChange(constants.map((c, i) => (i === index ? { ...c, name } : c)));
  };

  const setValue = (index: number, raw: string) => {
    const v = Number(raw);
    if (Number.isNaN(v)) return;
    onChange(constants.map((c, i) => (i === index ? { ...c, value: v } : c)));
  };

  const setDescription = (index: number, description: string) => {
    onChange(constants.map((c, i) => (i === index ? { ...c, description } : c)));
  };

  const remove = async (index: number) => {
    const c = constants[index];
    const ok = await confirm(
      `Delete constant "${c.name}"? Any formula that references it will become invalid until fixed.`,
      { title: "Delete constant", kind: "warning" },
    );
    if (!ok) return;
    onChange(constants.filter((_, i) => i !== index));
  };

  return (
    <fieldset>
      <legend>Constants ({constants.length})</legend>
      <p className="hint">
        Each constant name becomes a variable usable in any formula (task, project, or
        student). Use them like weights, e.g. <code>student_base * extra_credit</code>.
      </p>
      <table>
        <thead>
          <tr>
            <th>name (formula variable)</th>
            <th>value</th>
            <th>description</th>
            <th aria-label="actions" />
          </tr>
        </thead>
        <tbody>
          {constants.map((c, i) => (
            <tr key={i}>
              <td>
                <input
                  type="text"
                  value={c.name}
                  spellCheck={false}
                  onChange={(e) => rename(i, e.target.value)}
                />
              </td>
              <td>
                <input
                  type="number"
                  step="any"
                  value={c.value}
                  onChange={(e) => setValue(i, e.target.value)}
                />
              </td>
              <td>
                <input
                  type="text"
                  value={c.description}
                  onChange={(e) => setDescription(i, e.target.value)}
                />
              </td>
              <td>
                <button type="button" onClick={() => void remove(i)}>
                  Remove
                </button>
              </td>
            </tr>
          ))}
          {constants.length === 0 && (
            <tr>
              <td colSpan={4} className="hint">
                No constants defined yet.
              </td>
            </tr>
          )}
        </tbody>
      </table>
      <button type="button" onClick={addConstant}>
        Add constant
      </button>
    </fieldset>
  );
}
