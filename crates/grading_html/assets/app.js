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
    refresh();
  }

  function refresh() {
    GE.recompute(db, knobs);
    renderBanner();
    renderViews();
    renderFormula();
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
    inp.addEventListener('input', renderFormula);
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
      'SELECT student_id, base, stu_pen, ai_keep, contribution, final FROM derived_student ORDER BY project_id, student_id'
    );
    const ix = {};
    q.columns.forEach((c, i) => (ix[c] = i));
    const rows = q.rows.map((r) => {
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
      return [r[ix.student_id], r[ix.final], typeof v === 'number' ? round2(v) : String(v)];
    });
    out.innerHTML = tableHTML(['student', 'final', 'preview'], rows);
  }

  // ---- declarative views registry ----
  const VIEWS = [
    {
      id: 'student_grades',
      title: 'Student grades',
      chart: 'table',
      sql:
        'SELECT vs.project_name AS team, vs.student_id AS student, ds.final AS grade, ds.base, ' +
        'ds.stu_pen, ds.ai_keep, ds.contribution, vs.review_gate AS gate ' +
        'FROM v_student vs JOIN derived_student ds ON ds.student_id = vs.student_id ' +
        'AND ds.project_id = vs.project_id ORDER BY vs.project_id, grade DESC',
    },
    {
      id: 'team_summary',
      title: 'Team final grade',
      chart: 'bar',
      sql:
        'SELECT p.name AS team, dp.final AS final FROM derived_project dp ' +
        'JOIN project p ON p.project_id = dp.project_id ORDER BY final DESC',
    },
    {
      id: 'ai_keep_vs_grade',
      title: 'AI keep factor vs final grade',
      chart: 'scatter',
      sql: 'SELECT ai_keep, final FROM derived_student WHERE ai_keep IS NOT NULL',
    },
    {
      id: 'flags',
      title: 'Flags (sprint + artifact)',
      chart: 'table',
      sql:
        'SELECT project_id AS team, student_id AS student, source, sprint_id AS sprint, ' +
        'flag_type, severity FROM flag ORDER BY severity, project_id',
    },
    {
      id: 'llm_flags',
      title: 'LLM quality flags',
      chart: 'table',
      sql:
        'SELECT project_id AS team, student_id AS student, scope, category, severity, summary ' +
        'FROM llm_flag ORDER BY project_id',
    },
  ];

  function renderViews() {
    const container = byId('views');
    container.textContent = '';
    for (const v of VIEWS) {
      const card = document.createElement('section');
      card.className = 'view';
      card.id = 'view-' + v.id;
      const h = document.createElement('h3');
      h.textContent = v.title;
      card.appendChild(h);
      const body = document.createElement('div');
      body.className = 'view-body';
      let data;
      try {
        data = query(v.sql);
      } catch (e) {
        body.innerHTML = '<span class="err">' + esc(String(e)) + '</span>';
        card.appendChild(body);
        container.appendChild(card);
        continue;
      }
      if (typeof v.render === 'function') v.render(body, data);
      else if (v.chart === 'table') body.innerHTML = tableHTML(data.columns, data.rows);
      else body.innerHTML = svgChart(v.chart, data);
      card.appendChild(body);
      container.appendChild(card);
    }
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
