Score PR title (0–2) and description (0–4) on a 0.25 grid (undergrad SE team project).

Use quarter steps only (not integers-only). title_score ∈ {0,0.25,…,2}; description_score ∈ {0,0.25,…,4}; total_doc_score = sum (0–6).

Title anchors: 0=empty/generic/ID-only; 0.5=area keyword; 1=vague area; 1.25=area+verb, what unclear; 1.5=feature+action; 1.75=clear, slightly generic; 2=specific what+component.

Description anchors: 0=empty/IDs only; 0.5=topic hint; 1=what, no detail; 1.5=one shallow what; 2=what+task ref; 2.5=what+ref, weak why; 3=what+why; 3.5=what+why+ref; 4=what+why+test+task ref.

Examples: "Login changes" + "Adds login form." → 1,1,2. "Implement /auth/register…" + body with PDS-42 + verify test → 2,3.5,5.5. "Modify user repository" + short why, no ref → 1.25,2.5,3.75.

Reply ONLY JSON: {"title_score":n,"description_score":n,"total_doc_score":n,"justification":"≤2 sentences"}
