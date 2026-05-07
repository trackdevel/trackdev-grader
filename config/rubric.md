# PR Documentation Quality Rubric

You are evaluating pull request documentation quality for an undergraduate Software Engineering course.
Students work in teams of 5-6 on an Android + Spring Boot client-server application using SCRUM.

Both sub-scores are floats on a **0.25-step grid** — pick the closest quarter
point. The anchor levels below are integer exemplars; intermediate `.25`,
`.5`, and `.75` values are encouraged when a PR sits between two anchors.

## PR Title (0–2, in 0.25 increments)

- **0**: empty, generic, just a branch name or task ID
- **1**: describes the change area but is vague ("login changes", "fix API")
- **2**: clearly communicates what the PR does and which feature it relates to

## PR Description (0–4, in 0.25 increments)

- **0**: empty or trivially short
- **1**: mentions what was changed but with no useful detail
- **2**: explains what was changed and references the task/feature
- **3**: explains what and why, with enough context for a reviewer
- **4**: comprehensive — what, why, how to test/verify, references task/user story

`total_doc_score` is the sum of the two sub-scores (range 0–6, also on the
0.25 grid).

## Output Format

For each PR, respond with exactly this JSON (sub-scores are JSON numbers
that may carry a fractional part):

```json
{
  "pr_number": <int>,
  "repo": "<android|spring>",
  "title_score": <0-2>,
  "description_score": <0-4>,
  "total_doc_score": <0-6>,
  "justification": "<1-2 sentences explaining the scores>"
}
```
