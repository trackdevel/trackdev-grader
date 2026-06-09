/** Human-readable flag detail summaries (subset of grading_html/app.js). */

export function formatFlagDetails(flagType: string, details: string | null): string {
  if (!details) return "";
  let parsed: unknown;
  try {
    parsed = JSON.parse(details);
  } catch {
    return details;
  }
  if (typeof parsed !== "object" || parsed === null) return details;

  const v = parsed as Record<string, unknown>;

  switch (flagType) {
    case "CROSS_TEAM_SIMILARITY": {
      const pct = v.similarity_pct ?? v.similarity;
      const other = v.other_team ?? v.other_project;
      if (pct != null && other) return `${pct}% similar to ${other}`;
      break;
    }
    case "APPROVED_BROKEN_PR": {
      const num = v.pr_number ?? v.number;
      const title = v.pr_title ?? v.title;
      if (num != null) return `PR #${num}${title ? `: ${title}` : ""}`;
      break;
    }
    case "PR_DOES_NOT_COMPILE":
    case "SINGLE_COMMIT_DUMP":
    case "LAST_MINUTE_PR": {
      const num = v.pr_number ?? v.number;
      if (num != null) return `PR #${num}`;
      break;
    }
    default:
      break;
  }

  return JSON.stringify(parsed);
}

export function flagSeverityClass(severity: string): string {
  if (severity === "CRITICAL") return "flag-critical";
  if (severity === "WARNING") return "flag-warning";
  return "flag-info";
}
