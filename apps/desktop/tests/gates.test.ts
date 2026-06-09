import { describe, expect, it } from "vitest";

import type { ProjectDiagnostics } from "../src/data/diagnostics";
import type { RawProject } from "../src/data/types";
import {
  maxDeclaredLevel,
  projectReviewGate,
  studentReviewGate,
} from "../src/logic/gates";

const baseRaw = (overrides: Partial<RawProject> = {}): RawProject => ({
  project_id: 1,
  name: "Team",
  team_size: 2,
  axis: {
    documentation_raw: 0,
    doc_present: false,
    code_quality_raw: 0,
    cc_pct: 0,
    mutation_score: 0,
    cq_present: false,
    survival_raw: 0,
    surv_present: false,
    arch_crit_count: 0,
    arch_warn_count: 0,
    arch_present: false,
  },
  tasks: [],
  students: [],
  crit_findings: [],
  student_flags: [],
  ...overrides,
});

const emptyDiag = (): ProjectDiagnostics => ({
  flags: [],
  aiDetect: [],
  plagiarism: false,
});

describe("review gates", () => {
  it("returns NO_DELIVERY when effective points are zero", () => {
    const raw = baseRaw();
    expect(studentReviewGate(raw, emptyDiag(), "alice", 0)).toBe("NO_DELIVERY");
  });

  it("returns PLAGIARISM for project with cross-team flag", () => {
    const diag: ProjectDiagnostics = {
      ...emptyDiag(),
      plagiarism: true,
    };
    expect(projectReviewGate(diag)).toBe("PLAGIARISM");
    expect(studentReviewGate(baseRaw(), diag, "alice", 5)).toBe("PLAGIARISM");
  });

  it("returns AI_REVIEW for HIGH detect with low declared level", () => {
    const raw = baseRaw({
      tasks: [
        {
          assignee_id: "alice",
          raw_points: 5,
          ai_model: "Cap",
          ai_level: "A",
          declared: true,
        },
      ],
    });
    const diag: ProjectDiagnostics = {
      ...emptyDiag(),
      aiDetect: [{ student_id: "alice", risk_level: "HIGH", sprint_label: "S1" }],
    };
    expect(studentReviewGate(raw, diag, "alice", 5)).toBe("AI_REVIEW");
  });

  it("computes max declared level", () => {
    const tasks = [
      {
        assignee_id: "bob",
        raw_points: 1,
        ai_model: "Cap",
        ai_level: "C",
        declared: true,
      },
      {
        assignee_id: "bob",
        raw_points: 1,
        ai_model: "Cap",
        ai_level: "E",
        declared: true,
      },
    ];
    expect(maxDeclaredLevel(tasks, "bob")).toBe("E");
  });
});
