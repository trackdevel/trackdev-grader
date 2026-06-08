// grading.html — UI shell. Boots sql.js from the embedded wasm, opens the
// snapshot, recomputes via GradeEngine, and renders the parity banner, knobs
// panel, formula box and the declarative VIEWS. No network, no localStorage.
(function () {
  'use strict';
  const GE = window.GradeEngine;

  // The 25 scalar knobs the parity contract pins (ids: `knob-<name>`).
  const KNOB_GROUPS = [
    ['Project weights', ['w_doc', 'w_cq', 'w_surv', 'w_arch']],
    ['AI modulation', ['ai_strength', 'floor_keep', 'undeclared_model_m', 'undeclared_level_l']],
    [
      'Penalties',
      [
        'max_penalty_points',
        'student_penalty_cap',
        'crit_sa_points',
        'crit_cx_points',
        'crit_flag_points',
        'security_extra',
      ],
    ],
    [
      'Normalization',
      ['doc_max', 'mi_floor', 'mi_ceiling', 'cc_penalty', 'test_bonus', 'test_cap', 'surv_floor', 'surv_ceiling'],
    ],
    ['Architecture (live)', ['k_crit', 'k_warn', 'arch_norm']],
  ];
  const SCALAR_KNOBS = KNOB_GROUPS.flatMap((g) => g[1]);

  let db;
  let knobs;
  let defaultKnobs;
  const studentSort = { key: 'student', dir: 'asc' };
  const taskSort = { key: 'captured_at', dir: 'desc' };

  function b64ToBytes(b64) {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return bytes;
  }
  function debounce(fn, ms) {
    let t;
    return function () {
      clearTimeout(t);
      t = setTimeout(fn, ms);
    };
  }
  function byId(id) {
    return document.getElementById(id);
  }
  function esc(s) {
    return String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  }
  function round2(x) {
    return Math.round(x * 100) / 100;
  }
  function fmt(v) {
    if (v === null || v === undefined) return '';
    if (typeof v === 'number') return Number.isInteger(v) ? String(v) : String(round2(v));
    return esc(v);
  }

  // Run a query, returning { columns, rows }.
  function query(sql) {
    const res = db.exec(sql);
    return res.length ? { columns: res[0].columns, rows: res[0].values } : { columns: [], rows: [] };
  }

  async function boot() {
    const SQL = await initSqlJs({ wasmBinary: b64ToBytes(window.__SQL_WASM_B64__) });
    db = new SQL.Database(b64ToBytes(window.__SNAPSHOT_B64__));
    defaultKnobs = JSON.parse(byId('default-knobs').textContent);
    knobs = GE.knobsFromTables(db);
    GE.recompute(db, knobs);

    const m = query('SELECT generated_at, weights_version FROM meta').rows[0] || ['', ''];
    byId('meta-line').textContent = 'generated ' + m[0] + ' · weights ' + String(m[1]).slice(0, 12) + '…';

    buildKnobsPanel();
    buildFormulaBox();
    byId('reset-knobs').addEventListener('click', resetKnobs);
    bindNavigation();
    window.addEventListener('hashchange', onRouteChange);
    if (!location.hash || location.hash === '#') location.hash = '#/students';
    refresh();
  }

  function refresh() {
    GE.recompute(db, knobs);
    renderBanner();
    onRouteChange();
  }

  // Navigation only — no grade recompute (hash changes must stay snappy).
  function onRouteChange() {
    renderPage();
    const route = parseRoute();
    if (route.page === 'students' || route.page === 'projects') renderFormula();
  }

  function navigateTo(hash) {
    if (!hash) return;
    if (location.hash === hash) onRouteChange();
    else location.hash = hash;
  }

  // Intercept in-app hash links; native hash navigation is unreliable in some
  // file:// / embedded preview hosts.
  function bindNavigation() {
    document.body.addEventListener('click', function (e) {
      const link = e.target.closest('a[href^="#/"]');
      if (!link) return;
      e.preventDefault();
      navigateTo(link.getAttribute('href'));
    });
  }
  const debouncedRefresh = debounce(refresh, 150);

  function atDefaults() {
    if (knobs.penalty_mode !== defaultKnobs.penalty_mode) return false;
    for (const n of SCALAR_KNOBS) if (Math.abs(knobs[n] - defaultKnobs[n]) > 1e-12) return false;
    for (const k in defaultKnobs.models) if (Math.abs(knobs.models[k] - defaultKnobs.models[k]) > 1e-12) return false;
    for (const k in defaultKnobs.levels) if (Math.abs(knobs.levels[k] - defaultKnobs.levels[k]) > 1e-12) return false;
    return true;
  }

  function renderBanner() {
    const banner = byId('parity-banner');
    const res = GE.checkParity(db, knobs, knobs.decimals);
    if (atDefaults()) {
      if (res.ok) {
        banner.className = 'banner ok';
        banner.textContent =
          '✓ Parity verified — in-browser grades reproduce the Rust pipeline within 0.5·10⁻' +
          knobs.decimals +
          ' (max Δ ' +
          res.maxDelta.toExponential(1) +
          ').';
      } else {
        banner.className = 'banner error';
        banner.textContent =
          '⚠ PARITY BROKEN at default knobs — ' +
          res.offenders.length +
          ' row(s), max Δ ' +
          res.maxDelta +
          '. Release blocker: reconcile engine.js against the Rust model.';
      }
    } else {
      banner.className = 'banner tuned';
      banner.textContent =
        '● Knobs tuned — live what-if grades, NOT the official Rust-computed values. Use “Reset knobs” to restore defaults and re-verify parity.';
    }
  }

  // ---- knobs panel ----
  function buildKnobsPanel() {
    const panel = byId('knobs-panel');
    panel.textContent = '';
    const h = document.createElement('h2');
    h.textContent = 'Knobs';
    panel.appendChild(h);

    for (const group of KNOB_GROUPS) {
      const fs = document.createElement('fieldset');
      const lg = document.createElement('legend');
      lg.textContent = group[0];
      fs.appendChild(lg);
      for (const name of group[1]) fs.appendChild(scalarRow(name));
      panel.appendChild(fs);
    }

    const pm = document.createElement('fieldset');
    const pl = document.createElement('legend');
    pl.textContent = 'Penalty mode';
    pm.appendChild(pl);
    const sel = document.createElement('select');
    sel.id = 'penalty-mode-select';
    for (const opt of ['subtractive', 'off']) {
      const o = document.createElement('option');
      o.value = opt;
      o.textContent = opt;
      if (knobs.penalty_mode === opt) o.selected = true;
      sel.appendChild(o);
    }
    sel.addEventListener('change', function () {
      knobs.penalty_mode = sel.value;
      debouncedRefresh();
    });
    pm.appendChild(sel);
    panel.appendChild(pm);

    panel.appendChild(mapFieldset('AI model m (live for declared tasks)', 'model', knobs.models));
    panel.appendChild(mapFieldset('AI level l (live for declared tasks)', 'level', knobs.levels));
  }

  function numberInput(id, value, onChange) {
    const inp = document.createElement('input');
    inp.type = 'number';
    inp.step = 'any';
    inp.id = id;
    inp.value = value;
    inp.addEventListener('input', function () {
      const v = parseFloat(inp.value);
      if (!Number.isNaN(v)) onChange(v);
    });
    return inp;
  }
  function scalarRow(name) {
    const row = document.createElement('label');
    row.className = 'knob';
    const span = document.createElement('span');
    span.textContent = name;
    row.appendChild(span);
    row.appendChild(
      numberInput('knob-' + name, knobs[name], function (v) {
        knobs[name] = v;
        debouncedRefresh();
      })
    );
    return row;
  }
  function mapFieldset(title, prefix, map) {
    const fs = document.createElement('fieldset');
    fs.className = 'map';
    const lg = document.createElement('legend');
    lg.textContent = title;
    fs.appendChild(lg);
    for (const key of Object.keys(map)) {
      const row = document.createElement('label');
      row.className = 'knob';
      const span = document.createElement('span');
      span.textContent = key;
      row.appendChild(span);
      row.appendChild(
        numberInput(prefix + '-' + key, map[key], function (v) {
          map[key] = v;
          debouncedRefresh();
        })
      );
      fs.appendChild(row);
    }
    return fs;
  }
  function resetKnobs() {
    knobs = GE.knobsFromTables(db);
    buildKnobsPanel();
    refresh();
  }

  // ---- formula box (math.js, non-authoritative) ----
  function buildFormulaBox() {
    const box = byId('formula-box');
    box.textContent = '';
    const p = document.createElement('p');
    p.className = 'hint';
    p.textContent = 'math.js expression evaluated per student over: base, stu_pen, ai_keep, contribution, final.';
    const inp = document.createElement('input');
    inp.type = 'text';
    inp.id = 'formula-input';
    inp.value = 'min(10, base * 1.1 - stu_pen)';
    inp.addEventListener('input', function () {
      renderFormula();
      if (parseRoute().page === 'students') {
        const views = byId('views');
        if (views) renderStudentList(views);
      }
    });
    const out = document.createElement('div');
    out.id = 'formula-out';
    box.appendChild(p);
    box.appendChild(inp);
    box.appendChild(out);
  }
  function renderFormula() {
    const out = byId('formula-out');
    if (!out) return;
    const expr = byId('formula-input').value;
    let compiled;
    try {
      compiled = window.math.compile(expr);
    } catch (e) {
      out.innerHTML = '<span class="err">' + esc(String(e)) + '</span>';
      return;
    }
    const q = query(
      'SELECT s.project_id, s.student_id, s.full_name AS student, ds.base, ds.stu_pen, ' +
        'ds.ai_keep, ds.contribution, ds.final ' +
        'FROM derived_student ds JOIN student s ON s.student_id = ds.student_id ' +
        'AND s.project_id = ds.project_id ORDER BY ds.project_id, s.full_name'
    );
    const ix = {};
    q.columns.forEach((c, i) => (ix[c] = i));
    let html = '<table><thead><tr><th>student</th><th>final</th><th>preview</th></tr></thead><tbody>';
    for (const r of q.rows) {
      let v;
      try {
        v = compiled.evaluate({
          base: r[ix.base],
          stu_pen: r[ix.stu_pen],
          ai_keep: r[ix.ai_keep],
          contribution: r[ix.contribution],
          final: r[ix.final],
        });
      } catch (e) {
        v = 'err';
      }
      html +=
        '<tr><td>' +
        studentLink(r[ix.project_id], r[ix.student_id], r[ix.student]) +
        '</td><td>' +
        fmt(r[ix.final]) +
        '</td><td>' +
        fmt(typeof v === 'number' ? round2(v) : v) +
        '</td></tr>';
    }
    html += '</tbody></table>';
    if (!q.rows.length) html += '<p class="hint">no rows</p>';
    out.innerHTML = html;
  }

  // ---- hash routing: #/students | #/projects | #/student/:pid/:sid | #/project/:pid ----
  function parseRoute() {
    const raw = location.hash.replace(/^#\/?/, '');
    const parts = raw.split('/').filter(Boolean);
    if (!parts.length || parts[0] === 'students') return { page: 'students' };
    if (parts[0] === 'projects') return { page: 'projects' };
    if (parts[0] === 'student' && parts.length >= 3) {
      return { page: 'student', projectId: Number(parts[1]), studentId: decodeURIComponent(parts[2]) };
    }
    if (parts[0] === 'project' && parts.length >= 2) {
      return { page: 'project', projectId: Number(parts[1]) };
    }
    return { page: 'students' };
  }

  function updateNav(route) {
    const top = route.page === 'student' ? 'students' : route.page === 'project' ? 'projects' : route.page;
    document.querySelectorAll('#main-nav a').forEach(function (a) {
      a.classList.toggle('active', a.getAttribute('data-route') === top);
    });
    const formula = byId('formula-section');
    if (formula) formula.open = route.page === 'students' || route.page === 'projects';
  }

  function projectHref(pid) {
    return '#/project/' + pid;
  }
  function studentHref(pid, sid) {
    return '#/student/' + pid + '/' + encodeURIComponent(sid);
  }
  function projectLink(pid, label) {
    return (
      '<a class="entity-link" href="' +
      esc(projectHref(pid)) +
      '">' +
      esc(label) +
      '</a>'
    );
  }
  function studentLink(pid, sid, label) {
    if (pid == null || sid == null || sid === '') return esc(label);
    return (
      '<a class="entity-link" href="' +
      esc(studentHref(pid, sid)) +
      '">' +
      esc(label) +
      '</a>'
    );
  }

  const TRACKDEV_TASK_URL = 'https://trackdev.org/dashboard/tasks/';
  function taskLink(taskId, label) {
    if (taskId == null) return esc(label);
    const url = TRACKDEV_TASK_URL + taskId;
    return (
      '<a class="entity-link external" href="' +
      esc(url) +
      '" target="_blank" rel="noopener noreferrer">' +
      esc(label) +
      '</a>'
    );
  }

  function sortIndicator(state, key) {
    if (state.key !== key) return '';
    return state.dir === 'asc' ? ' ▲' : ' ▼';
  }

  function sortRows(rows, state) {
    const dir = state.dir === 'asc' ? 1 : -1;
    const key = state.key;
    const numeric = { grade: 1, preview: 1, raw_points: 1, captured_at: 1 };
    return rows.slice().sort(function (a, b) {
      let av = a[key];
      let bv = b[key];
      if (key === 'captured_at') {
        const at = av ? String(av) : '';
        const bt = bv ? String(bv) : '';
        if (at !== bt) return (at < bt ? -1 : 1) * dir;
        return String(a.task).localeCompare(String(b.task)) * dir;
      }
      if (numeric[key]) {
        av = Number(av);
        bv = Number(bv);
        if (Number.isNaN(av)) av = 0;
        if (Number.isNaN(bv)) bv = 0;
        return (av - bv) * dir;
      }
      return String(av ?? '').localeCompare(String(bv ?? ''), undefined, { sensitivity: 'base' }) * dir;
    });
  }

  function attachSortHandlers(tableId, state, rerender) {
    const table = byId(tableId);
    if (!table) return;
    table.querySelectorAll('th[data-sort]').forEach(function (th) {
      th.addEventListener('click', function () {
        const key = th.getAttribute('data-sort');
        if (state.key === key) state.dir = state.dir === 'asc' ? 'desc' : 'asc';
        else {
          state.key = key;
          state.dir = key === 'grade' || key === 'preview' || key === 'raw_points' || key === 'captured_at' ? 'desc' : 'asc';
        }
        rerender();
      });
    });
  }

  function formulaPreviewExpr() {
    const inp = byId('formula-input');
    return inp && inp.value.trim() ? inp.value.trim() : 'min(10, base * 1.1 - stu_pen)';
  }

  function evalPreview(row) {
    try {
      const compiled = window.math.compile(formulaPreviewExpr());
      const v = compiled.evaluate({
        base: row.base,
        stu_pen: row.stu_pen,
        ai_keep: row.ai_keep,
        contribution: row.contribution,
        final: row.grade,
      });
      return typeof v === 'number' ? round2(v) : v;
    } catch (e) {
      return 'err';
    }
  }

  function gradeTreeSummary(n) {
    let s = '<span class="tree-title">' + esc(n.title) + '</span>';
    if (n.value !== undefined && n.value !== null) s += ' <span class="tree-val">' + fmt(n.value) + '</span>';
    if (n.formula) s += '<span class="tree-formula-inline"> — ' + esc(n.formula) + '</span>';
    return s;
  }

  function renderGradeTree(node) {
    if (!node) return '<p class="hint">no explanation</p>';
    function walk(n, depth) {
      const hasKids = n.children && n.children.length;
      const expandable = hasKids || n.detail;
      if (!expandable) {
        return '<li class="tree-leaf"><div class="tree-summary">' + gradeTreeSummary(n) + '</div></li>';
      }
      const openAttr = depth === 0 ? ' open' : '';
      let h = '<li><details class="tree-details depth-' + depth + '"' + openAttr + '>';
      h += '<summary class="tree-summary">' + gradeTreeSummary(n) + '</summary>';
      h += '<div class="tree-body">';
      if (n.detail) h += '<div class="tree-detail">' + esc(n.detail) + '</div>';
      if (hasKids) {
        h +=
          '<ul class="grade-tree">' +
          n.children
            .map(function (c) {
              return walk(c, depth + 1);
            })
            .join('') +
          '</ul>';
      }
      h += '</div></details></li>';
      return h;
    }
    return '<ul class="grade-tree root">' + walk(node, 0) + '</ul>';
  }

  function renderTasksTable(data, tableId, rerender) {
    const rows = sortRows(rowsAsObjects(data), taskSort);
    if (!rows.length) return '<p class="hint">no tasks</p>';
    let h = '<table id="' + esc(tableId) + '" class="sortable-table"><thead><tr>';
    const cols = [
      ['task', 'task'],
      ['raw_points', 'raw_points'],
      ['ai_model', 'ai_model'],
      ['ai_level', 'ai_level'],
      ['declared', 'declared'],
    ];
    for (const c of cols) {
      const sortable = c[0] !== 'ai_level' && c[0] !== 'declared';
      const extra = c[0] === 'task' ? ' (updated)' : '';
      h +=
        '<th' +
        (sortable ? ' class="sortable" data-sort="' + esc(c[0]) + '"' : '') +
        '>' +
        esc(c[1] + extra) +
        (sortable ? esc(sortIndicator(taskSort, c[0])) : '') +
        '</th>';
    }
    h += '</tr></thead><tbody>';
    for (const r of rows) {
      const title = r.captured_at ? esc(r.task) + ' · ' + esc(String(r.captured_at).slice(0, 10)) : null;
      h += '<tr><td title="' + (title || '') + '">' + taskLink(r.task_id, r.task) + '</td>';
      h += '<td>' + fmt(r.raw_points) + '</td>';
      h += '<td>' + fmt(r.ai_model) + '</td>';
      h += '<td>' + fmt(r.ai_level) + '</td>';
      h += '<td>' + fmt(r.declared) + '</td></tr>';
    }
    h += '</tbody></table>';
  }

  function mountTasksTable(container, data, tableId) {
    function rerender() {
      container.innerHTML = renderTasksTable(data, tableId, rerender);
      attachSortHandlers(tableId, taskSort, rerender);
    }
    rerender();
  }

  function rowsAsObjects(data) {
    return data.rows.map(function (row) {
      const o = {};
      data.columns.forEach(function (c, i) {
        o[c] = row[i];
      });
      return o;
    });
  }

  function renderPage() {
    const route = parseRoute();
    updateNav(route);
    const container = byId('views');
    container.textContent = '';
    try {
      if (route.page === 'students') renderStudentList(container);
      else if (route.page === 'projects') renderProjectList(container);
      else if (route.page === 'student') renderStudentDetail(container, route.projectId, route.studentId);
      else if (route.page === 'project') renderProjectDetail(container, route.projectId);
    } catch (e) {
      container.innerHTML = '<span class="err">' + esc(String(e)) + '</span>';
    }
  }

  function sectionBlock(title, inner) {
    return '<section class="detail-section"><h3>' + esc(title) + '</h3>' + inner + '</section>';
  }

  function kvTable(pairs) {
    if (!pairs.length) return '<p class="hint">no data</p>';
    let h = '<table class="kv-table"><tbody>';
    for (const p of pairs) {
      h += '<tr><th>' + esc(p[0]) + '</th><td>' + fmt(p[1]) + '</td></tr>';
    }
    h += '</tbody></table>';
    return h;
  }

  function renderStudentList(container) {
    const data = query(
      'SELECT s.project_id, s.student_id, p.name AS team, s.full_name AS student, ' +
        'ds.final AS grade, ds.base, ds.stu_pen, ds.ai_keep, ds.contribution, ds.review_gate AS gate ' +
        'FROM student s JOIN project p ON p.project_id = s.project_id ' +
        'JOIN derived_student ds ON ds.student_id = s.student_id AND ds.project_id = s.project_id'
    );
    let rows = rowsAsObjects(data);
    rows = rows.map(function (r) {
      r.preview = evalPreview(r);
      return r;
    });
    rows = sortRows(rows, studentSort);

    let body =
      '<section class="view"><h3>All students</h3>' +
      '<p class="hint">Click column headers to sort. Preview uses the formula box expression.</p>' +
      '<div class="view-body"><table id="student-list-table" class="sortable-table"><thead><tr>';
    body +=
      '<th>team</th>' +
      '<th class="sortable" data-sort="student">student' +
      esc(sortIndicator(studentSort, 'student')) +
      '</th>' +
      '<th class="sortable" data-sort="grade">grade' +
      esc(sortIndicator(studentSort, 'grade')) +
      '</th>' +
      '<th>base</th><th>stu_pen</th><th>ai_keep</th><th>contribution</th><th>gate</th>' +
      '<th class="sortable" data-sort="preview">preview' +
      esc(sortIndicator(studentSort, 'preview')) +
      '</th>';
    body += '</tr></thead><tbody>';
    for (const r of rows) {
      body +=
        '<tr><td>' +
        projectLink(r.project_id, r.team) +
        '</td><td>' +
        studentLink(r.project_id, r.student_id, r.student) +
        '</td>';
      for (const k of ['grade', 'base', 'stu_pen', 'ai_keep', 'contribution', 'gate']) {
        body += '<td>' + fmt(r[k]) + '</td>';
      }
      body += '<td>' + fmt(r.preview) + '</td></tr>';
    }
    body += '</tbody></table>';
    if (!rows.length) body += '<p class="hint">no students</p>';
    body += '</div></section>';
    container.innerHTML = body;
    attachSortHandlers('student-list-table', studentSort, function () {
      renderStudentList(container);
    });
  }

  function renderProjectList(container) {
    const data = query(
      'SELECT p.project_id, p.name AS team, p.team_size, dp.final AS grade, dp.quality, ' +
        'dp.q_pen AS quality_penalized, dp.ai_factor, rp.review_gate AS gate ' +
        'FROM project p JOIN derived_project dp ON dp.project_id = p.project_id ' +
        'LEFT JOIN reference_project rp ON rp.project_id = p.project_id ORDER BY grade DESC'
    );
    const rows = rowsAsObjects(data);
    let body = '<section class="view"><h3>All projects</h3><div class="view-body"><table><thead><tr>';
    for (const c of ['team', 'grade', 'quality', 'quality_penalized', 'ai_factor', 'team_size', 'gate']) {
      body += '<th>' + esc(c) + '</th>';
    }
    body += '</tr></thead><tbody>';
    for (const r of rows) {
      body += '<tr><td>' + projectLink(r.project_id, r.team) + '</td>';
      for (const k of ['grade', 'quality', 'quality_penalized', 'ai_factor', 'team_size', 'gate']) {
        body += '<td>' + fmt(r[k]) + '</td>';
      }
      body += '</tr>';
    }
    body += '</tbody></table>';
    if (!rows.length) body += '<p class="hint">no projects</p>';
    body += '</div></section>';
    container.innerHTML = body;
  }

  function renderStudentDetail(container, projectId, studentId) {
    const info = rowsAsObjects(
      query(
        'SELECT s.full_name AS student, p.name AS team, p.project_id, s.student_id, ' +
          'ds.final, ds.base, ds.stu_pen, ds.ai_keep, ds.contribution, ds.review_gate ' +
          'FROM student s JOIN project p ON p.project_id = s.project_id ' +
          'JOIN derived_student ds ON ds.student_id = s.student_id AND ds.project_id = s.project_id ' +
          'WHERE s.project_id = ' +
          projectId +
          " AND s.student_id = '" +
          studentId.replace(/'/g, "''") +
          "'"
      )
    )[0];
    if (!info) {
      container.innerHTML = '<p class="err">Student not found.</p>';
      return;
    }

    const sidEsc = studentId.replace(/'/g, "''");
    const tasks = query(
      "SELECT t.task_id, COALESCE(lt.label, 'task-' || t.task_id) AS task, t.raw_points, " +
        't.ai_model, t.ai_level, t.captured_at, CASE WHEN t.declared = 1 THEN ' +
        "'yes' ELSE 'no' END AS declared " +
        'FROM task t LEFT JOIN label_task lt ON lt.task_id = t.task_id ' +
        'WHERE t.project_id = ' +
        projectId +
        " AND t.assignee_id = '" +
        sidEsc +
        "'"
    );
    const flags = query(
      'SELECT ls.label AS sprint, f.source, f.flag_type, f.severity, f.details ' +
        'FROM flag f LEFT JOIN label_sprint ls ON ls.sprint_id = f.sprint_id ' +
        'WHERE f.project_id = ' +
        projectId +
        " AND f.student_id = '" +
        sidEsc +
        "' ORDER BY f.severity, f.flag_type"
    );
    const ai = query(
      'SELECT ls.label AS sprint, a.risk_level ' +
        'FROM ai_detect a LEFT JOIN label_sprint ls ON ls.sprint_id = a.sprint_id ' +
        'WHERE a.project_id = ' +
        projectId +
        " AND a.student_id = '" +
        sidEsc +
        "' ORDER BY ls.label"
    );
    const llm = query(
      'SELECT ls.label AS sprint, l.scope, COALESCE(lt.label, l.target_ref) AS target, ' +
        'l.category, l.severity, l.summary ' +
        'FROM llm_flag l LEFT JOIN label_sprint ls ON ls.sprint_id = l.sprint_id ' +
        'LEFT JOIN label_target lt ON lt.target_ref = l.target_ref ' +
        'WHERE l.project_id = ' +
        projectId +
        " AND l.student_id = '" +
        sidEsc +
        "' ORDER BY l.severity"
    );

    let html =
      '<div class="detail-page">' +
      '<a class="back-link" href="#/students">← All students</a>' +
      '<h2>' +
      esc(info.student) +
      '</h2>' +
      '<p class="subtitle">Team: ' +
      projectLink(info.project_id, info.team) +
      '</p>';

    html += sectionBlock(
      'Grade breakdown',
      kvTable([
        ['Final grade', info.final],
        ['Base grade', info.base],
        ['Student penalty', info.stu_pen],
        ['AI keep factor', info.ai_keep],
        ['Contribution share', info.contribution],
        ['Review gate', info.review_gate],
      ])
    );
    const tree = GE.explainStudent(db, knobs, projectId, studentId);
    html += sectionBlock('How the final grade is computed', renderGradeTree(tree));
    html += '<section class="detail-section" id="tasks-section"><h3>Tasks</h3>';
    html += '<p class="hint">Default order: last AI-declaration capture date (newest first), then task key. Click headers to re-sort.</p>';
    html += '<div id="tasks-table-host"></div></section>';
    html += sectionBlock('Flags', tableHTML(flags.columns, flags.rows));
    html += sectionBlock('AI detection', tableHTML(ai.columns, ai.rows));
    html += sectionBlock('LLM quality flags', tableHTML(llm.columns, llm.rows));
    html += '</div>';
    container.innerHTML = html;
    const tasksHost = byId('tasks-table-host');
    if (tasksHost) mountTasksTable(tasksHost, tasks, 'student-tasks-table');
  }

  function renderProjectDetail(container, projectId) {
    const info = rowsAsObjects(
      query(
        'SELECT p.name AS team, p.project_id, p.team_size, dp.final, dp.quality, dp.q_pen, ' +
          'dp.ai_factor, dp.sum_raw, dp.sum_eff, dp.mean_raw, rp.review_gate ' +
          'FROM project p JOIN derived_project dp ON dp.project_id = p.project_id ' +
          'LEFT JOIN reference_project rp ON rp.project_id = p.project_id ' +
          'WHERE p.project_id = ' +
          projectId
      )
    )[0];
    if (!info) {
      container.innerHTML = '<p class="err">Project not found.</p>';
      return;
    }

    const axis = rowsAsObjects(query('SELECT * FROM project_axis WHERE project_id = ' + projectId))[0] || {};
    const students = query(
      'SELECT s.student_id, s.full_name AS student, ds.final AS grade, ds.base, ' +
        'ds.contribution, ds.review_gate AS gate ' +
        'FROM student s JOIN derived_student ds ON ds.student_id = s.student_id AND ds.project_id = s.project_id ' +
        'WHERE s.project_id = ' +
        projectId +
        ' ORDER BY grade DESC'
    );
    const crit = query(
      'SELECT c.repo_full_name AS repo, c.kind, c.rule_id, c.severity, c.category ' +
        'FROM crit_flag c WHERE c.project_id = ' +
        projectId +
        ' ORDER BY c.repo_full_name'
    );
    const llm = query(
      'SELECT s.full_name AS student, ls.label AS sprint, l.scope, ' +
        'COALESCE(lt.label, l.target_ref) AS target, l.category, l.severity, l.summary ' +
        'FROM llm_flag l JOIN project p ON p.project_id = l.project_id ' +
        'LEFT JOIN student s ON s.student_id = l.student_id AND s.project_id = l.project_id ' +
        'LEFT JOIN label_sprint ls ON ls.sprint_id = l.sprint_id ' +
        'LEFT JOIN label_target lt ON lt.target_ref = l.target_ref ' +
        'WHERE l.project_id = ' +
        projectId +
        ' ORDER BY l.severity'
    );

    let html =
      '<div class="detail-page">' +
      '<a class="back-link" href="#/projects">← All projects</a>' +
      '<h2>' +
      esc(info.team) +
      '</h2>' +
      '<p class="subtitle">Team size: ' +
      fmt(info.team_size) +
      '</p>';

    html += sectionBlock(
      'Team grade',
      kvTable([
        ['Final grade', info.final],
        ['Composite quality', info.quality],
        ['After penalties', info.q_pen],
        ['Team AI factor', info.ai_factor],
        ['Sum raw points', info.sum_raw],
        ['Sum effective points', info.sum_eff],
        ['Mean raw (per seat)', info.mean_raw],
        ['Review gate', info.review_gate],
      ])
    );
    html += sectionBlock(
      'Quality axes',
      kvTable([
        ['Documentation score', axis.documentation_score],
        ['Code quality score', axis.code_quality_score],
        ['Survival score', axis.survival_score],
        ['Architecture score', axis.architecture_score],
        ['CC %', axis.cc_pct],
        ['Mutation score', axis.mutation_score],
        ['Arch crit / warn', (axis.arch_crit_count || 0) + ' / ' + (axis.arch_warn_count || 0)],
      ])
    );

    const stuRows = rowsAsObjects(students);
    let stuBody = '<table><thead><tr>';
    for (const c of ['student', 'grade', 'base', 'contribution', 'gate']) {
      stuBody += '<th>' + esc(c) + '</th>';
    }
    stuBody += '</tr></thead><tbody>';
    for (const r of stuRows) {
      stuBody +=
        '<tr><td>' +
        studentLink(projectId, r.student_id, r.student) +
        '</td>';
      for (const k of ['grade', 'base', 'contribution', 'gate']) {
        stuBody += '<td>' + fmt(r[k]) + '</td>';
      }
      stuBody += '</tr>';
    }
    stuBody += '</tbody></table>';
    if (!stuRows.length) stuBody += '<p class="hint">no students</p>';
    html += sectionBlock('Students (summary)', stuBody);
    html += sectionBlock('Critical findings', tableHTML(crit.columns, crit.rows));
    html += sectionBlock('LLM quality flags', tableHTML(llm.columns, llm.rows));
    html += '</div>';
    container.innerHTML = html;
  }

  // ---- renderers ----
  function tableHTML(columns, rows) {
    let h = '<table><thead><tr>';
    for (const c of columns) h += '<th>' + esc(c) + '</th>';
    h += '</tr></thead><tbody>';
    for (const r of rows) {
      h += '<tr>';
      for (const cell of r) h += '<td>' + fmt(cell) + '</td>';
      h += '</tr>';
    }
    h += '</tbody></table>';
    if (!rows.length) h += '<p class="hint">no rows</p>';
    return h;
  }

  // Minimal SVG renderer (built as markup to avoid namespace handling).
  function svgChart(kind, data) {
    const rows = data.rows;
    if (!rows.length) return '<p class="hint">no rows</p>';
    const W = 560,
      H = 260,
      padL = 46,
      padR = 14,
      padT = 12,
      padB = 46;
    const x0 = padL,
      y0 = H - padB,
      plotW = W - padL - padR,
      plotH = H - padT - padB;
    let body =
      '<line x1="' + x0 + '" y1="' + padT + '" x2="' + x0 + '" y2="' + y0 + '" class="axis"/>' +
      '<line x1="' + x0 + '" y1="' + y0 + '" x2="' + (W - padR) + '" y2="' + y0 + '" class="axis"/>';

    function yTicks(max) {
      let s = '';
      for (let i = 0; i <= 4; i++) {
        const y = y0 - (i / 4) * plotH;
        s +=
          '<line x1="' + x0 + '" y1="' + y.toFixed(1) + '" x2="' + (W - padR) + '" y2="' + y.toFixed(1) + '" class="grid"/>' +
          '<text x="' + (x0 - 6) + '" y="' + (y + 3).toFixed(1) + '" class="ylab" text-anchor="end">' +
          round2((max * i) / 4) +
          '</text>';
      }
      return s;
    }

    if (kind === 'bar' || kind === 'hist') {
      let labels, vals;
      if (kind === 'hist') {
        const xs = rows.map((r) => Number(r[0]) || 0);
        const min = Math.min.apply(null, xs),
          max = Math.max.apply(null, xs);
        const span = max - min || 1,
          bins = 10,
          counts = new Array(bins).fill(0);
        xs.forEach((v) => {
          let b = Math.floor(((v - min) / span) * bins);
          if (b >= bins) b = bins - 1;
          if (b < 0) b = 0;
          counts[b]++;
        });
        labels = counts.map((_, i) => round2(min + (span * i) / bins));
        vals = counts;
      } else {
        labels = rows.map((r) => r[0]);
        vals = rows.map((r) => Number(r[1]) || 0);
      }
      const max = Math.max.apply(null, [1].concat(vals));
      const bw = plotW / vals.length;
      vals.forEach((v, i) => {
        const hgt = (v / max) * plotH;
        const x = x0 + i * bw + bw * 0.15;
        const w = bw * 0.7;
        const y = y0 - hgt;
        body +=
          '<rect x="' + x.toFixed(1) + '" y="' + y.toFixed(1) + '" width="' + w.toFixed(1) + '" height="' + hgt.toFixed(1) +
          '" class="bar"><title>' + esc(labels[i]) + ': ' + v + '</title></rect>' +
          '<text x="' + (x + w / 2).toFixed(1) + '" y="' + (y0 + 13) + '" class="xlab" text-anchor="middle">' +
          esc(String(labels[i]).slice(0, 8)) + '</text>';
      });
      body = yTicks(max) + body;
    } else {
      // scatter / line
      const xs = rows.map((r) => Number(r[0]) || 0);
      const ys = rows.map((r) => Number(r[1]) || 0);
      const xmin = Math.min.apply(null, xs),
        xmax = Math.max.apply(null, xs);
      const ymin = Math.min.apply(null, [0].concat(ys)),
        ymax = Math.max.apply(null, [1].concat(ys));
      const sx = (x) => x0 + ((x - xmin) / (xmax - xmin || 1)) * plotW;
      const sy = (y) => y0 - ((y - ymin) / (ymax - ymin || 1)) * plotH;
      body = yTicks(ymax) + body;
      if (kind === 'line') {
        const pts = xs
          .map((x, i) => [sx(x), sy(ys[i])])
          .sort((a, b) => a[0] - b[0])
          .map((p) => p[0].toFixed(1) + ',' + p[1].toFixed(1))
          .join(' ');
        body += '<polyline points="' + pts + '" class="line"/>';
      } else {
        xs.forEach((x, i) => {
          body +=
            '<circle cx="' + sx(x).toFixed(1) + '" cy="' + sy(ys[i]).toFixed(1) + '" r="3" class="pt">' +
            '<title>' + round2(x) + ', ' + round2(ys[i]) + '</title></circle>';
        });
      }
    }
    return '<svg viewBox="0 0 ' + W + ' ' + H + '" class="chart" preserveAspectRatio="xMidYMid meet">' + body + '</svg>';
  }

  boot().catch(function (e) {
    const banner = byId('parity-banner');
    banner.className = 'banner error';
    banner.textContent = 'boot failed: ' + e;
  });
})();
