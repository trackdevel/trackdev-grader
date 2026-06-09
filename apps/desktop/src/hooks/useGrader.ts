import { useCallback, useEffect, useMemo, useRef, useState } from "react";

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

export function useGrader(rawProjects: RawProject[]): GraderState {
  const bundledDefault = useMemo(() => loadBundledDefault(), []);
  const [spec, setSpecState] = useState<GradeSpec>(() => loadBundledDefault());
  const [specPath, setSpecPath] = useState<string | null>(null);
  const [validationError, setValidationError] = useState<string | null>(null);
  const [recomputeError, setRecomputeError] = useState<string | null>(null);
  const [grades, setGrades] = useState<Map<number, GradeOutput>>(new Map());
  const [loading, setLoading] = useState(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const probeRef = useRef<RawProject | null>(null);

  const edited = isEditedSpec(spec, bundledDefault);

  const runRecompute = useCallback(
    async (nextSpec: GradeSpec, projects: RawProject[]) => {
      const validation = validateSpec(nextSpec);
      if (!validation.ok) {
        setValidationError(validation.message);
        return;
      }

      if (projects.length === 0) {
        setValidationError(null);
        setRecomputeError(null);
        return;
      }

      const probe = probeRef.current ?? projects[0];
      const dry = await dryRunSpec(nextSpec, probe, async (raw, s) => {
        await initEngine();
        return (await recompute(raw, s)) as GradeOutput;
      });
      if (!dry.ok) {
        setValidationError(dry.message);
        return;
      }

      setValidationError(null);
      setLoading(true);
      const result = await recomputeAll(projects, nextSpec);
      setLoading(false);
      setRecomputeError(result.error);
      if (result.grades.size > 0) {
        setGrades(new Map(result.grades));
      }
    },
    [],
  );

  useEffect(() => {
    if (rawProjects.length > 0) {
      probeRef.current = rawProjects[0];
    }
  }, [rawProjects]);

  useEffect(() => {
    if (timerRef.current) clearTimeout(timerRef.current);
    timerRef.current = setTimeout(() => {
      void runRecompute(spec, rawProjects);
    }, DEBOUNCE_MS);
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, [spec, rawProjects, runRecompute]);

  const setSpec = useCallback((next: GradeSpec) => {
    setSpecState(next);
  }, []);

  const resetSpec = useCallback(() => {
    // Reset restores the bundled grading *logic* but preserves professor-entered
    // manual fields (definitions + per-project values), which are data, not logic.
    setSpecState((prev) => {
      const d = loadBundledDefault();
      return { ...d, manual_fields: prev.manual_fields ?? d.manual_fields };
    });
    setSpecPath(null);
  }, []);

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
