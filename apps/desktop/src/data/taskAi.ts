/** One row from `task_ai_usage` (model, level, declared flag). */
export type AiUsageRow = {
  model_value: string | null;
  level_value: string | null;
  declared: number | null;
};

export type ResolvedAiUsage = {
  ai_model: string | null;
  ai_level: string | null;
  declared: boolean;
};

/**
 * "Set" means the both-present gate: declared === 1 AND model AND level.
 * Mirror of `grading_projection::raw::resolve_task_ai` / grade_core policy.
 */
export function isAiUsageSet(
  model: string | null,
  level: string | null,
  declared: number | null,
): boolean {
  return (declared ?? 0) === 1 && model !== null && level !== null;
}

/**
 * Resolve effective AI usage: own attribute → parent USER_STORY → undeclared.
 */
export function resolveEffectiveAiUsage(own: AiUsageRow, parent: AiUsageRow): ResolvedAiUsage {
  if (isAiUsageSet(own.model_value, own.level_value, own.declared)) {
    return {
      ai_model: own.model_value,
      ai_level: own.level_value,
      declared: true,
    };
  }
  if (isAiUsageSet(parent.model_value, parent.level_value, parent.declared)) {
    return {
      ai_model: parent.model_value,
      ai_level: parent.level_value,
      declared: true,
    };
  }
  return { ai_model: null, ai_level: null, declared: false };
}
