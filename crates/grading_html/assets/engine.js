// grading.html — JS port of the Rust grade arithmetic.
//
// Parity is the contract: with the snapshot's default knobs, every value this
// engine writes into `derived_project` / `derived_student` must reproduce the
// Rust-computed `reference_project` / `reference_student` within 0.5·10^-decimals.
// The arithmetic below is transcribed line-for-line from grade.rs, normalize.rs,
// modulation.rs, aggregate.rs and penalty.rs. Do not "simplify" it.
//
// Loadable two ways: as a classic <script> (defines a global `GradeEngine`) and
// via `new Function(src + 'return GradeEngine;')` in the Node parity harness.

const GradeEngine = (function () {
  function clamp(x, lo, hi) {
    return Math.max(lo, Math.min(hi, x));
  }
  function clamp01(x) {
    return clamp(x, 0, 1);
  }

  // Read every row of a (possibly parameterized) query as plain objects.
  function rows(db, sql, params) {
    const stmt = db.prepare(sql);
    if (params) stmt.bind(params);
    const out = [];
    while (stmt.step()) out.push(stmt.getAsObject());
    stmt.free();
    return out;
  }

  // modulation.rs::keep — keep_t = 1 − (1 − floor_keep)·strength·m·l.
  function keep(m, l, strength, floorKeep) {
    return 1 - (1 - floorKeep) * strength * m * l;
  }

  // aggregate.rs::load_task_points — a task counts as declared only when
  // declared==1 AND BOTH model and level are present; otherwise both scalars
  // fall to the undeclared defaults (the both-present gate).
  function keepForTask(t, k) {
    const declaredFull = t.declared === 1 && t.ai_model != null && t.ai_level != null;
    const m = declaredFull ? k.models[t.ai_model] ?? 1.0 : k.undeclared_model_m;
    const l = declaredFull ? k.levels[t.ai_level] ?? 1.0 : k.undeclared_level_l;
    return keep(m, l, k.ai_strength, k.floor_keep);
  }

  // normalize.rs axis scores (present-gated; null when the axis is absent).
  function docScore(ax, k) {
    if (ax.documentation_present !== 1 || ax.documentation_raw == null) return null;
    return clamp(10 * clamp01(ax.documentation_raw / k.doc_max), 0, 10);
  }
  function cqScore(ax, k) {
    if (ax.code_quality_present !== 1 || ax.code_quality_raw == null) return null;
    const base = 10 * clamp01((ax.code_quality_raw - k.mi_floor) / (k.mi_ceiling - k.mi_floor));
    const ccAdj = ax.cc_pct != null ? k.cc_penalty * (ax.cc_pct / 100) : 0;
    const testAdj =
      ax.mutation_score != null ? Math.min(k.test_cap, k.test_bonus * ax.mutation_score) : 0;
    return clamp(base - ccAdj + testAdj, 0, 10);
  }
  function survScore(ax, k) {
    if (ax.survival_present !== 1 || ax.survival_raw == null) return null;
    return clamp(10 * clamp01((ax.survival_raw - k.surv_floor) / (k.surv_ceiling - k.surv_floor)), 0, 10);
  }
  // LIVE: recomputed from raw crit/warn counts so k_crit/k_warn/arch_norm move
  // the architecture axis (the workbook bakes only the final density).
  function archScore(ax, k) {
    if (ax.architecture_present !== 1) return null;
    const density = (k.k_crit * ax.arch_crit_count + k.k_warn * ax.arch_warn_count) / k.arch_norm;
    return clamp(10 - Math.min(10, density), 0, 10);
  }

  // normalize.rs::quality_composite — renormalized over PRESENT axes only.
  function composite(ax, k) {
    const axes = [
      [docScore(ax, k), k.w_doc, ax.documentation_present],
      [cqScore(ax, k), k.w_cq, ax.code_quality_present],
      [survScore(ax, k), k.w_surv, ax.survival_present],
      [archScore(ax, k), k.w_arch, ax.architecture_present],
    ];
    let sumW = 0;
    let sumWs = 0;
    for (const [score, weight, present] of axes) {
      if (present === 1 && score !== null) {
        sumW += weight;
        sumWs += weight * score;
      }
    }
    return sumW > 0 ? sumWs / sumW : 0;
  }

  // penalty.rs::project_penalty — zeroed unless penalty_mode == 'subtractive'.
  function projectPenalty(db, projectId, k) {
    if (k.penalty_mode !== 'subtractive') return 0;
    let total = 0;
    for (const f of rows(db, 'SELECT kind, category FROM crit_flag WHERE project_id = ?', [projectId])) {
      if (f.kind === 'static_analysis') {
        total += k.crit_sa_points + (f.category === 'security' ? k.security_extra : 0);
      } else if (f.kind === 'complexity') {
        total += k.crit_cx_points;
      }
    }
    return Math.min(k.max_penalty_points, total);
  }

  // penalty.rs::student_penalty — CRITICAL sprint + artifact flags, capped.
  function studentPenalty(db, studentId, projectId, k) {
    if (k.penalty_mode !== 'subtractive') return 0;
    const r = rows(
      db,
      "SELECT COUNT(*) AS n FROM flag WHERE student_id = ? AND project_id = ? AND severity = 'CRITICAL'",
      [studentId, projectId]
    )[0];
    const n = r ? r.n : 0;
    return Math.min(k.student_penalty_cap, k.crit_flag_points * n);
  }

  // grade.rs::round_grade — round to `decimals`, then optional grid snap.
  function roundGrade(value, decimals, quantize) {
    const factor = Math.pow(10, decimals);
    let r = Math.round(value * factor) / factor;
    if (quantize > 0) r = Math.round(r / quantize) * quantize;
    return r;
  }

  // Gates are knob-independent; copied from reference_student, not recomputed.
  function reviewGate(db, studentId, projectId) {
    const r = rows(
      db,
      'SELECT review_gate FROM reference_student WHERE student_id = ? AND project_id = ?',
      [studentId, projectId]
    )[0];
    return r ? r.review_gate : null;
  }

  // Read the 25 scalar knobs + maps + meta controls from the snapshot.
  function knobsFromTables(db) {
    const k = {};
    for (const r of rows(db, 'SELECT name, value FROM weights')) k[r.name] = r.value;
    k.models = {};
    for (const r of rows(db, 'SELECT name, m FROM models')) k.models[r.name] = r.m;
    k.levels = {};
    for (const r of rows(db, 'SELECT name, l FROM levels')) k.levels[r.name] = r.l;
    const meta = rows(db, 'SELECT decimals, quantize_final, penalty_mode FROM meta')[0] || {};
    k.decimals = meta.decimals;
    k.quantize_final = meta.quantize_final;
    k.penalty_mode = meta.penalty_mode;
    return k;
  }

  // Recompute every project/student grade under `k` and (re)materialize the
  // derived_* tables that the views join.
  function recompute(db, k) {
    db.run('DROP TABLE IF EXISTS derived_project');
    db.run('DROP TABLE IF EXISTS derived_student');
    db.run(
      'CREATE TABLE derived_project (project_id INTEGER, quality REAL, q_pen REAL, ' +
        'ai_factor REAL, sum_raw REAL, sum_eff REAL, mean_raw REAL, final REAL)'
    );
    db.run(
      'CREATE TABLE derived_student (student_id TEXT, project_id INTEGER, ai_keep REAL, ' +
        'contribution REAL, base REAL, stu_pen REAL, final REAL, review_gate TEXT)'
    );

    for (const proj of rows(db, 'SELECT project_id, team_size FROM project')) {
      const pid = proj.project_id;

      const tasks = rows(
        db,
        'SELECT assignee_id, raw_points, ai_model, ai_level, declared FROM task WHERE project_id = ?',
        [pid]
      );
      const perStudent = {};
      let sumRaw = 0;
      let sumEff = 0;
      for (const t of tasks) {
        const eff = t.raw_points * keepForTask(t, k);
        sumRaw += t.raw_points;
        sumEff += eff;
        const sp = perStudent[t.assignee_id] || (perStudent[t.assignee_id] = { raw: 0, eff: 0 });
        sp.raw += t.raw_points;
        sp.eff += eff;
      }

      const teamSize = Math.max(1, proj.team_size || 0);
      const meanRaw = sumRaw > 0 ? sumRaw / teamSize : 0;
      const aiFactor = sumRaw > 0 ? sumEff / sumRaw : 1;

      const ax = rows(db, 'SELECT * FROM project_axis WHERE project_id = ?', [pid])[0] || {};
      const q = composite(ax, k);
      const qPen = clamp(q - projectPenalty(db, pid, k), 0, 10);
      const projectFinal = roundGrade(qPen * aiFactor, k.decimals, k.quantize_final);

      db.run('INSERT INTO derived_project VALUES (?,?,?,?,?,?,?,?)', [
        pid,
        q,
        qPen,
        aiFactor,
        sumRaw,
        sumEff,
        meanRaw,
        projectFinal,
      ]);

      for (const st of rows(db, 'SELECT student_id FROM student WHERE project_id = ?', [pid])) {
        const sid = st.student_id;
        const sp = perStudent[sid] || { raw: 0, eff: 0 };
        const aiKeep = sp.raw > 0 ? sp.eff / sp.raw : null;
        const contribution = sumEff > 0 ? sp.eff / sumEff : null;
        const base = meanRaw > 0 ? (qPen * sp.eff) / meanRaw : 0;
        const stuPen = studentPenalty(db, sid, pid, k);
        const final =
          sp.eff <= 0 ? 0 : roundGrade(clamp(base - stuPen, 0, 10), k.decimals, k.quantize_final);
        db.run('INSERT INTO derived_student VALUES (?,?,?,?,?,?,?,?)', [
          sid,
          pid,
          aiKeep,
          contribution,
          base,
          stuPen,
          final,
          reviewGate(db, sid, pid),
        ]);
      }
    }
  }

  // Compare derived finals against the baked reference finals.
  function checkParity(db, k, decimals) {
    const tol = 0.5 * Math.pow(10, -decimals);
    let maxDelta = 0;
    const offenders = [];
    const consider = (kind, id, projectId, d, r) => {
      const delta = Math.abs((d ?? 0) - (r ?? 0));
      if (delta > maxDelta) maxDelta = delta;
      if (delta > tol) offenders.push({ kind, id, project_id: projectId, derived: d, reference: r, delta });
    };
    for (const row of rows(
      db,
      'SELECT ds.student_id, ds.project_id, ds.final AS d, rs.final_grade AS r ' +
        'FROM derived_student ds JOIN reference_student rs ' +
        'ON rs.student_id = ds.student_id AND rs.project_id = ds.project_id'
    )) {
      consider('student', row.student_id, row.project_id, row.d, row.r);
    }
    for (const row of rows(
      db,
      'SELECT dp.project_id, dp.final AS d, rp.final_grade AS r ' +
        'FROM derived_project dp JOIN reference_project rp ON rp.project_id = dp.project_id'
    )) {
      consider('project', row.project_id, row.project_id, row.d, row.r);
    }
    return { ok: offenders.length === 0, maxDelta, offenders };
  }

  function r4(x) {
    return Math.round(x * 10000) / 10000;
  }

  function axisExplain(ax, k) {
    const out = [];
    if (ax.documentation_present === 1 && ax.documentation_raw != null) {
      const s = docScore(ax, k);
      out.push({
        title: 'Documentation',
        formula: '10 × clamp(doc_raw / doc_max)',
        value: s,
        detail: 'raw=' + r4(ax.documentation_raw) + ', w=' + k.w_doc,
      });
    }
    if (ax.code_quality_present === 1 && ax.code_quality_raw != null) {
      const s = cqScore(ax, k);
      out.push({
        title: 'Code quality',
        formula: 'MI mapped + test bonus − CC penalty',
        value: s,
        detail:
          'MI=' +
          r4(ax.code_quality_raw) +
          ', cc%=' +
          (ax.cc_pct != null ? r4(ax.cc_pct) : '—') +
          ', mut=' +
          (ax.mutation_score != null ? r4(ax.mutation_score) : '—') +
          ', w=' +
          k.w_cq,
      });
    }
    if (ax.survival_present === 1 && ax.survival_raw != null) {
      const s = survScore(ax, k);
      out.push({
        title: 'Survival',
        formula: '10 × clamp((raw − floor) / (ceiling − floor))',
        value: s,
        detail: 'raw=' + r4(ax.survival_raw) + ', w=' + k.w_surv,
      });
    }
    if (ax.architecture_present === 1) {
      const s = archScore(ax, k);
      const density = (k.k_crit * ax.arch_crit_count + k.k_warn * ax.arch_warn_count) / k.arch_norm;
      out.push({
        title: 'Architecture',
        formula: '10 − min(10, density)',
        value: s,
        detail:
          'crit=' +
          ax.arch_crit_count +
          ', warn=' +
          ax.arch_warn_count +
          ', density=' +
          r4(density) +
          ', w=' +
          k.w_arch,
      });
    }
    return out;
  }

  // Explain how the live engine computed one student's grade (tree for the UI).
  function explainStudent(db, k, projectId, studentId) {
    const pid = projectId;
    const sid = studentId;
    const ax = rows(db, 'SELECT * FROM project_axis WHERE project_id = ?', [pid])[0] || {};
    const proj = rows(db, 'SELECT team_size FROM project WHERE project_id = ?', [pid])[0] || {};
    const teamSize = Math.max(1, proj.team_size || 0);

    const allTasks = rows(
      db,
      'SELECT assignee_id, raw_points, ai_model, ai_level, declared FROM task WHERE project_id = ?',
      [pid]
    );
    let sumRaw = 0;
    let sumEff = 0;
    const perStudent = {};
    for (const t of allTasks) {
      const keep = keepForTask(t, k);
      const eff = t.raw_points * keep;
      sumRaw += t.raw_points;
      sumEff += eff;
      const sp = perStudent[t.assignee_id] || (perStudent[t.assignee_id] = { raw: 0, eff: 0, tasks: [] });
      sp.raw += t.raw_points;
      sp.eff += eff;
      if (t.assignee_id === sid) {
        const declaredFull = t.declared === 1 && t.ai_model != null && t.ai_level != null;
        const m = declaredFull ? k.models[t.ai_model] ?? 1.0 : k.undeclared_model_m;
        const l = declaredFull ? k.levels[t.ai_level] ?? 1.0 : k.undeclared_level_l;
        sp.tasks.push({
          raw: t.raw_points,
          keep,
          eff,
          declaredFull,
          m,
          l,
          model: t.ai_model,
          level: t.ai_level,
        });
      }
    }

    const sp = perStudent[sid] || { raw: 0, eff: 0, tasks: [] };
    const meanRaw = sumRaw > 0 ? sumRaw / teamSize : 0;
    const q = composite(ax, k);
    const projPen = projectPenalty(db, pid, k);
    const qPen = clamp(q - projPen, 0, 10);
    const base = meanRaw > 0 ? (qPen * sp.eff) / meanRaw : 0;
    const stuPen = studentPenalty(db, sid, pid, k);
    const preClamp = sp.eff <= 0 ? 0 : base - stuPen;
    const final = sp.eff <= 0 ? 0 : roundGrade(clamp(preClamp, 0, 10), k.decimals, k.quantize_final);
    const aiKeep = sp.raw > 0 ? sp.eff / sp.raw : null;
    const contribution = sumEff > 0 ? sp.eff / sumEff : null;

    const taskChildren = sp.tasks.map(function (t, i) {
      return {
        title: 'Task ' + (i + 1),
        formula: 'effective = raw × keep',
        value: r4(t.eff),
        children: [
          { title: 'raw_points', value: t.raw },
          {
            title: 'keep',
            formula: '1 − (1−floor_keep)×α×m×l',
            value: r4(t.keep),
            children: [
              {
                title: t.declaredFull ? 'declared AI usage' : 'undeclared (default m, l)',
                detail: t.declaredFull
                  ? 'model=' + t.model + ' (m=' + t.m + '), level=' + t.level + ' (l=' + t.l + ')'
                  : 'm=' + t.m + ', l=' + t.l,
              },
            ],
          },
        ],
      };
    });

    const axes = axisExplain(ax, k);
    const axisChildren = axes.map(function (a) {
      return { title: a.title, formula: a.formula, value: a.value, detail: a.detail };
    });

    const critFlags = rows(db, 'SELECT kind, category FROM crit_flag WHERE project_id = ?', [pid]);
    const critN = rows(
      db,
      "SELECT COUNT(*) AS n FROM flag WHERE student_id = ? AND project_id = ? AND severity = 'CRITICAL'",
      [sid, pid]
    )[0];

    if (sp.eff <= 0) {
      return {
        title: 'Final grade',
        value: 0,
        formula: 'NO_DELIVERY — effective points = 0 → grade 0',
        children: [{ title: 'Student effective points', value: 0 }],
      };
    }

    return {
      title: 'Final grade',
      value: final,
      formula: 'round(clamp(base − student_penalty, 0, 10))',
      children: [
        {
          title: 'base',
          formula: '(quality_penalized × student_effective) / mean_raw_per_seat',
          value: r4(base),
          children: [
            {
              title: 'quality_penalized (q_pen)',
              formula: 'clamp(quality_composite − project_penalty, 0, 10)',
              value: r4(qPen),
              children: [
                {
                  title: 'quality_composite',
                  formula: 'renormalized weighted mean of present axes',
                  value: r4(q),
                  children: axisChildren,
                },
                {
                  title: 'project_penalty',
                  value: r4(projPen),
                  detail:
                    k.penalty_mode !== 'subtractive'
                      ? 'penalty_mode ≠ subtractive → 0'
                      : critFlags.length +
                        ' crit finding(s); cap=' +
                        k.max_penalty_points,
                },
              ],
            },
            { title: 'student_effective', value: r4(sp.eff), children: taskChildren },
            {
              title: 'mean_raw_per_seat',
              formula: 'team_sum_raw / team_size',
              value: r4(meanRaw),
              detail: 'sum_raw=' + r4(sumRaw) + ', seats=' + teamSize,
            },
          ],
        },
        {
          title: 'student_penalty',
          value: r4(stuPen),
          detail:
            k.penalty_mode !== 'subtractive'
              ? 'penalty_mode ≠ subtractive → 0'
              : (critN ? critN.n : 0) +
                ' CRITICAL flag(s) × ' +
                k.crit_flag_points +
                ', cap=' +
                k.student_penalty_cap,
        },
        { title: 'ai_keep (student)', value: aiKeep != null ? r4(aiKeep) : '—', formula: 'student_eff / student_raw' },
        {
          title: 'contribution share',
          value: contribution != null ? r4(contribution) : '—',
          formula: 'student_eff / team_eff',
          detail: 'team_eff=' + r4(sumEff),
        },
      ],
    };
  }

  return { keep, knobsFromTables, recompute, checkParity, explainStudent };
})();

if (typeof window !== 'undefined') window.GradeEngine = GradeEngine;
