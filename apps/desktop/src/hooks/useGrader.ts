import { useCallback, useEffect, useMemo, useState } from "react";

import { loadBundledDefault } from "../config/load";
import { dryRunSpec, isEditedSpec, validateSpec } from "../config/validate";
import type { GradeOutput, GradeSpec, RawProject } from "../data/types";
import { clearLastGoodGrades, initEngine, recompute, recomputeAll } from "../engine";

const DEBOUNCE_MS = 350;

export type GraderState = {
  spec: GradeSpec;
  bundledDefault: GradeSpec;
  validationError: string | null;
  recomputeError: string | null;
  grades: Map<number, GradeOutput>;
  loading: boolean;
  /** Spec differs from the bundled standard (drives the parity banner). */
  edited: boolean;
  /** Spec has changes not yet written to disk (drives the Save indicator). */
  dirty: boolean;
  specPath: string | null;
  /** Apply an in-app edit: recomputes and marks the spec dirty. */
  setSpec: (spec: GradeSpec) => void;
  /** Load a spec from disk (session or Open spec…): clean, not dirty. */
  loadSpec: (spec: GradeSpec, path: string | null) => void;
  resetSpec: () => void;
  setSpecPath: (path: string | null) => void;
  /** Clear the dirty flag after a successful save. */
  markSaved: () => void;
};

async function gradeProbe(raw: RawProject, spec: GradeSpec): Promise<GradeOutput> {
  await initEngine();
  return (await recompute(raw, spec)) as GradeOutput;
}

export function useGrader(rawProjects: RawProject[]): GraderState {
  const bundledDefault = useMemo(() => loadBundledDefault(), []);
  const [spec, setSpecState] = useState<GradeSpec>(() => loadBundledDefault());
  const [specPath, setSpecPath] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);
  const [recomputeError, setRecomputeError] = useState<string | null>(null);
  const [grades, setGrades] = useState<Map<number, GradeOutput>>(new Map());
  const [loading, setLoading] = useState(false);

  const edited = isEditedSpec(spec, bundledDefault);

  // An in-app edit always dirties the spec; loading from disk does not.
  const setSpec = useCallback((next: GradeSpec) => {
    setSpecState(next);
    setDirty(true);
  }, []);

  const loadSpec = useCallback((next: GradeSpec, path: string | null) => {
    setSpecState(next);
    setSpecPath(path);
    setDirty(false);
  }, []);

  const markSaved = useCallback(() => setDirty(false), []);

  // Debounced recompute. The `cancelled` flag makes superseded runs drop
  // their results, so rapid spec edits can't commit grades out of order.
  useEffect(() => {
    let cancelled = false;
    // Drop cached grades whenever inputs change so a failed engine run cannot
    // keep showing scores from a prior spec/db (looks like "grades didn't update").
    clearLastGoodGrades();
    setRecomputeError(null);
    setGrades(new Map());

    const run = async () => {
      const validation = validateSpec(spec);
      if (!validation.ok) {
        if (!cancelled) setValidationError(validation.message);
        return;
      }
      if (rawProjects.length === 0) {
        if (!cancelled) {
          setValidationError(null);
          setRecomputeError(null);
        }
        return;
      }

      const dry = await dryRunSpec(spec, rawProjects[0], gradeProbe);
      if (cancelled) return;
      if (!dry.ok) {
        setValidationError(dry.message);
        return;
      }

      setValidationError(null);
      setLoading(true);
      const result = await recomputeAll(rawProjects, spec);
      if (cancelled) return;
      setLoading(false);
      if (result.error) {
        const hint = result.error.includes("unknown variable")
          ? " — restart the app to reload the grading engine (stale WASM)"
          : "";
        setRecomputeError(result.error + hint);
        return;
      }
      setRecomputeError(null);
      setGrades(new Map(result.grades));
    };

    const timer = setTimeout(() => void run(), DEBOUNCE_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [spec, rawProjects]);

  const resetSpec = useCallback(() => {
    // Reset restores the bundled grading *logic* but preserves professor-entered
    // manual fields (definitions + per-project values), which are data, not logic.
    // It changes the in-memory spec, so it counts as an unsaved edit.
    setSpecState((prev) => {
      const d = loadBundledDefault();
      return {
        ...d,
        manual_fields: prev.manual_fields ?? d.manual_fields,
        constants: prev.constants ?? d.constants,
      };
    });
    setSpecPath(null);
    setDirty(true);
  }, []);

  return {
    spec,
    bundledDefault,
    validationError,
    recomputeError,
    grades,
    loading,
    edited,
    dirty,
    specPath,
    setSpec,
    loadSpec,
    resetSpec,
    setSpecPath,
    markSaved,
  };
}
