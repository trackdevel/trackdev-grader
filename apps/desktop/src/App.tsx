import { useCallback, useMemo, useState } from "react";

import { useGrader } from "./hooks/useGrader";
import { useHashRoute } from "./hooks/useHashRoute";
import { checkParity } from "./logic/parity";
import DbPicker from "./views/DbPicker";
import ParityBanner from "./views/ParityBanner";
import ProjectDetail from "./views/ProjectDetail";
import ProjectList from "./views/ProjectList";
import SpecEditor from "./views/SpecEditor";
import StudentDetail from "./views/StudentDetail";
import StudentList from "./views/StudentList";
import type { LoadedDb } from "./data/types";
import "./styles.css";

export default function App() {
  const [loadedDb, setLoadedDb] = useState<LoadedDb | null>(null);
  const grader = useGrader(loadedDb?.projects ?? []);
  const { route } = useHashRoute();

  const parity = useMemo(
    () => checkParity(grader.spec, grader.grades, grader.bundledDefault),
    [grader.spec, grader.grades, grader.bundledDefault],
  );

  const handleDbLoaded = useCallback((db: LoadedDb) => {
    setLoadedDb(db);
  }, []);

  const topNav =
    route.page === "student" ? "students" : route.page === "project" ? "projects" : route.page;

  return (
    <main className="app">
      <header>
        <h1>TrackDev Grader</h1>
        <p className="subtitle">Offline grading.db viewer with live formula tuning.</p>
      </header>

      <ParityBanner parity={parity} />

      <nav id="main-nav" className="main-nav">
        <a
          className={topNav === "students" ? "active" : undefined}
          href="#/students"
          data-route="students"
        >
          Students
        </a>
        <a
          className={topNav === "projects" ? "active" : undefined}
          href="#/projects"
          data-route="projects"
        >
          Projects
        </a>
      </nav>

      <DbPicker onLoaded={handleDbLoaded} />

      <details className="spec-panel">
        <summary>Grading spec editor</summary>
        <SpecEditor
          spec={grader.spec}
          validationError={grader.validationError}
          edited={grader.edited}
          specPath={grader.specPath}
          onChange={grader.setSpec}
          onReset={grader.resetSpec}
          onSpecPath={grader.setSpecPath}
        />
      </details>

      {grader.recomputeError && (
        <p className="error">Engine: {grader.recomputeError} (showing last-good grades)</p>
      )}
      {grader.loading && <p className="meta">Recomputing grades…</p>}

      <div id="views" className="views">
        {loadedDb ? (
          <>
            {route.page === "students" && (
              <StudentList db={loadedDb} grades={grader.grades} />
            )}
            {route.page === "projects" && (
              <ProjectList db={loadedDb} grades={grader.grades} />
            )}
            {route.page === "student" && (
              <StudentDetail
                db={loadedDb}
                grades={grader.grades}
                projectId={route.projectId}
                studentId={route.studentId}
              />
            )}
            {route.page === "project" && (
              <ProjectDetail
                db={loadedDb}
                grades={grader.grades}
                projectId={route.projectId}
              />
            )}
          </>
        ) : (
          <p className="meta">Open a grading.db to browse students and projects.</p>
        )}
      </div>
    </main>
  );
}
