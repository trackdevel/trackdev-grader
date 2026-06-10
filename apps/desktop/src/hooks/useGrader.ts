import { useEffect, useMemo, useState } from "react";

import { loadBundledDefault } from "../config/load";
import { dryRunSpec, isEditedSpec, validateSpec } from "../config/validate";
import type { GradeOutput, GradeSpec, RawProject } from "../data/types";
import { initEngine, recompute, recomputeAll } from "../engine";

const DEBOUNCE_MS = 350;

export type GraderState = {
  spec: GradeSpec;
  bundledDefault: GradeSpec;
  validationError: string | null;
  recomputeError: string | null;
  grades: Map<number, GradeOutput>;
  loading: boolean;
  edited: boolean;
  specPath: string | null;
  setSpec: (spec: GradeSpec) => void;
  resetSpec: () => void;
  setSpecPath: (path: string | null) => void;
};

async function gradeProbe(raw: RawProject, spec: GradeSpec): Promise<GradeOutput> {
  await initEngine();
  return (await recompute(raw, spec)) as GradeOutput;
}

export function useGrader(rawProjects: RawProject[]): GraderState {
  const bundledDefault = useMemo(() => loadBundledDefault(), []);
  const [spec, setSpec] = useState<GradeSpec>(() => loadBundledDefault());
  const [specPath, setSpecPath] = useState<string | null>(null);
  const [validationError, setValidationError] = useState<string | null>(null);
  const [recomputeError, setRecomputeError] = useState<string | null>(null);
  const [grades, setGrades] = useState<Map<number, GradeOutput>>(new Map());
  const [loading, setLoading] = useState(false);

  const edited = isEditedSpec(spec, bundledDefault);

  // Debounced recompute. The `cancelled` flag makes superseded runs drop
  // their results, so rapid spec edits can't commit grades out of order.
  useEffect(() => {
    let cancelled = false;

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
      setRecomputeError(result.error);
      if (result.grades.size > 0) {
        setGrades(new Map(result.grades));
      }
    };

    const timer = setTimeout(() => void run(), DEBOUNCE_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [spec, rawProjects]);

  const resetSpec = () => {
    // Reset restores the bundled grading *logic* but preserves professor-entered
    // manual fields (definitions + per-project values), which are data, not logic.
    setSpec((prev) => {
      const d = loadBundledDefault();
      return { ...d, manual_fields: prev.manual_fields ?? d.manual_fields };
    });
    setSpecPath(null);
  };

  return {
    spec,
    bundledDefault,
    validationError,
    recomputeError,
    grades,
    loading,
    edited,
    specPath,
    setSpec,
    resetSpec,
    setSpecPath,
  };
}
