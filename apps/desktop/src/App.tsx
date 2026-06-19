import { useEffect, useMemo, useState } from "react";

import { loadAppConfigFromCwd } from "./config/appConfig";
import { useGrader } from "./hooks/useGrader";
import { topTabOf, useHashRoute, type TopTab } from "./hooks/useHashRoute";
import { checkParity } from "./logic/parity";
import ConfigToolbar from "./views/ConfigToolbar";
import DbPicker from "./views/DbPicker";
import FormulaView from "./views/FormulaView";
import ParityBanner from "./views/ParityBanner";
import ProjectDetail from "./views/ProjectDetail";
import ProjectList from "./views/ProjectList";
import StudentDetail from "./views/StudentDetail";
import StudentList from "./views/StudentList";
import type { GradeSpec, LoadedDb, RawProject } from "./data/types";
import "./styles.css";

const NO_PROJECTS: RawProject[] = [];

const TABS: Array<{ tab: TopTab; href: string; label: string }> = [
  { tab: "students", href: "#/students", label: "Students" },
  { tab: "projects", href: "#/projects", label: "Projects" },
  { tab: "formula", href: "#/formula", label: "Formula" },
];

export default function App() {
  const [loadedDb, setLoadedDb] = useState<LoadedDb | null>(null);
  const [appConfigPath, setAppConfigPath] = useState<string | null>(null);
  const [bootError, setBootError] = useState<string | null>(null);

  const grader = useGrader(loadedDb?.projects ?? NO_PROJECTS);
  const { loadSpec } = grader;
  const { route } = useHashRoute();
  const activeTab = topTabOf(route);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const applied = await loadAppConfigFromCwd();
        if (cancelled || !applied) return;
        setAppConfigPath(applied.configPath);
        if (applied.db) setLoadedDb(applied.db);
        if (applied.spec) {
          loadSpec(applied.spec, applied.specPath);
        }
      } catch (e) {
        if (!cancelled) {
          setBootError(e instanceof Error ? e.message : String(e));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [loadSpec]);

  const parity = useMemo(
    () => checkParity(grader.spec, grader.grades, grader.bundledDefault),
    [grader.spec, grader.grades, grader.bundledDefault],
  );

  const applyConfigSession = (result: {
    configPath: string;
    db: LoadedDb | null;
    spec: GradeSpec | null;
    specPath: string | null;
  }) => {
    if (result.db) setLoadedDb(result.db);
    if (result.spec) {
      loadSpec(result.spec, result.specPath);
    }
    setAppConfigPath(result.configPath);
  };

  return (
    <main className="app">
      <header>
        <h1>TrackDev Grader</h1>
        <p className="subtitle">Offline grading.db viewer with live formula tuning.</p>
        <DbPicker dbPath={loadedDb?.path ?? null} onLoaded={setLoadedDb} />
        <ConfigToolbar
          appConfigPath={appConfigPath}
          loadedDb={loadedDb}
          spec={grader.spec}
          edited={grader.edited}
          dirty={grader.dirty}
          specPath={grader.specPath}
          onConfigApplied={applyConfigSession}
          onConfigPath={setAppConfigPath}
          onSpecPath={grader.setSpecPath}
          onSaved={grader.markSaved}
        />
        {bootError && <p className="error">Startup config: {bootError}</p>}
      </header>

      <ParityBanner parity={parity} />

      <nav id="main-nav" className="main-nav">
        {TABS.map(({ tab, href, label }) => (
          <a key={tab} className={activeTab === tab ? "active" : undefined} href={href}>
            {label}
          </a>
        ))}
      </nav>

      {grader.validationError && (
        <p className="error">Spec validation: {grader.validationError}</p>
      )}
      {grader.recomputeError && (
        <p className="error">Engine: {grader.recomputeError} (showing last-good grades)</p>
      )}
      {grader.loading && <p className="meta">Recomputing grades…</p>}

      <div id="views" className="views">
        {route.page === "formula" ? (
          <FormulaView
            spec={grader.spec}
            projects={loadedDb?.projects ?? NO_PROJECTS}
            validationError={grader.validationError}
            edited={grader.edited}
            specPath={grader.specPath}
            onChange={grader.setSpec}
            onReset={grader.resetSpec}
            onLoadSpec={grader.loadSpec}
          />
        ) : loadedDb === null ? (
          <p className="meta">Open a grading.db to browse students and projects.</p>
        ) : route.page === "students" ? (
          <StudentList db={loadedDb} grades={grader.grades} />
        ) : route.page === "projects" ? (
          <ProjectList db={loadedDb} grades={grader.grades} spec={grader.spec} />
        ) : route.page === "student" ? (
          <StudentDetail
            key={`${route.projectId}/${route.studentId}`}
            db={loadedDb}
            grades={grader.grades}
            projectId={route.projectId}
            studentId={route.studentId}
          />
        ) : (
          <ProjectDetail
            key={route.projectId}
            db={loadedDb}
            grades={grader.grades}
            spec={grader.spec}
            projectId={route.projectId}
          />
        )}
      </div>
    </main>
  );
}
