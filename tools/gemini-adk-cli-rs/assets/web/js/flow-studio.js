/* ================================================================
   Flow Studio — drag-and-drop editor for JSON-authored governed flows.

   The document being edited IS a FlowAppSpec: the exact JSON that
   `POST /api/flows/validate` checks and a `flow-studio` session runs.
   Node positions are editor-local (localStorage), never written into
   the exported spec.
   ================================================================ */
(() => {
  'use strict';

  // ── Document state ─────────────────────────────────────────────
  const blankSpec = () => ({
    name: '',
    description: '',
    instruction: '',
    greeting: null,
    modality: 'text',
    voice: null,
    tools: [],
    state: {},
    computed: [],
    watch: [],
    patterns: [],
    flow: { steps: [], constraints: [], ambient: [] },
  });

  let spec = blankSpec();
  let layout = {};           // stepId -> {x, y}
  let selectedId = null;
  let zoom = 1;
  let pan = { x: 40, y: 40 };
  let ws = null;             // live run socket
  let liveStatus = null;     // last FlowStatus payload

  // ── DOM handles ────────────────────────────────────────────────
  const $ = (id) => document.getElementById(id);
  const canvasWrap = $('fs-canvas-wrap');
  const canvas = $('fs-canvas');
  const edgesSvg = $('fs-edges');
  const nodesEl = $('fs-nodes');

  // ── Helpers ────────────────────────────────────────────────────
  const steps = () => spec.flow.steps;
  const findStep = (id) => steps().find((s) => s.id === id);
  // `after` entries are strings (unconditional) or {step, when} (conditional).
  const depStep = (d) => (typeof d === 'string' ? d : d.step);
  const depWhen = (d) => (typeof d === 'string' ? null : (d.when || null));

  function guardSummary(g) {
    if (g == null) return '';
    if (g === 'always') return 'always';
    const [k, v] = Object.entries(g)[0] || [];
    switch (k) {
      case 'is_true': return `is_true(${v})`;
      case 'is_set': return `is_set(${v})`;
      case 'eq': return `eq(${v[0]})`;
      case 'captured': return `captured(${v.join(', ')})`;
      case 'called_ok': return `called_ok(${v})`;
      case 'done': return `done(${v})`;
      case 'all': return v.map(guardSummary).join(' & ');
      case 'any': return v.map(guardSummary).join(' | ');
      case 'not': return `!${guardSummary(v)}`;
      default: return JSON.stringify(g);
    }
  }

  function walkGuardRename(g, from, to) {
    if (g == null || g === 'always') return g;
    const [k, v] = Object.entries(g)[0] || [];
    if (k === 'done' && v === from) return { done: to };
    if (k === 'all' || k === 'any') return { [k]: v.map((c) => walkGuardRename(c, from, to)) };
    if (k === 'not') return { not: walkGuardRename(v, from, to) };
    return g;
  }

  function uniqueStepId(base) {
    let id = base; let n = 1;
    while (findStep(id)) id = `${base}_${++n}`;
    return id;
  }

  function persist() {
    try {
      localStorage.setItem('fs-spec', JSON.stringify(spec));
      localStorage.setItem('fs-layout', JSON.stringify(layout));
    } catch (_) { /* private mode */ }
  }

  function restore() {
    try {
      const s = localStorage.getItem('fs-spec');
      const l = localStorage.getItem('fs-layout');
      if (s) spec = JSON.parse(s);
      if (l) layout = JSON.parse(l);
      normalizeSpec();
    } catch (_) { spec = blankSpec(); layout = {}; }
  }

  function normalizeSpec() {
    if (!spec || typeof spec !== 'object') spec = blankSpec();
    spec.tools = spec.tools || [];
    spec.flow = spec.flow || {};
    spec.flow.steps = spec.flow.steps || [];
    spec.flow.constraints = spec.flow.constraints || [];
    spec.flow.ambient = spec.flow.ambient || [];
    spec.patterns = spec.patterns || [];
    spec.state = spec.state || {};
    spec.computed = spec.computed || [];
    spec.watch = spec.watch || [];
    spec.modality = spec.modality || 'text';
    for (const s of steps()) {
      s.after = s.after || [];
      s.allow = s.allow || [];
      s.deny = s.deny || [];
      if (!layout[s.id]) layout[s.id] = { x: 0, y: 0 };
    }
  }

  /** Re-render everything, mark validation stale, save. */
  function commit({ relayout = false } = {}) {
    normalizeSpec();
    if (relayout) autoLayout();
    renderCanvas();
    refreshStateKeys();
    setBadge('idle', 'not validated');
    const jsonText = $('fs-json-text');
    if (document.activeElement !== jsonText) syncJsonPane();
    persist();
  }

  /** Every state key the document declares or writes — feeds the guard
      editors' autocomplete datalist. */
  function refreshStateKeys() {
    const list = $('fs-state-keys');
    if (!list) return;
    const keys = new Set(Object.keys(spec.state || {}));
    for (const t of spec.tools || []) {
      for (const k of Object.keys(t.set_state || {})) keys.add(k);
      if (t.save_response_as) keys.add(t.save_response_as);
    }
    for (const c of spec.computed || []) keys.add(c.key);
    for (const e of spec.extract || []) {
      keys.add(e.name);
      for (const p of e.promote || []) keys.add(p.to || p.field);
    }
    for (const m of (spec.memory && spec.memory.slots) || []) keys.add(m.to);
    list.innerHTML = '';
    [...keys].sort().forEach((k) => {
      const o = document.createElement('option');
      o.value = k;
      list.append(o);
    });
  }

  // ── Auto-layout (topological layering) ─────────────────────────
  function autoLayout() {
    const ids = steps().map((s) => s.id);
    const depth = {};
    const depthOf = (id, seen = new Set()) => {
      if (depth[id] !== undefined) return depth[id];
      if (seen.has(id)) return 0; // cycle — validation reports it
      seen.add(id);
      const s = findStep(id);
      const deps = (s?.after || []).map(depStep).filter((d) => ids.includes(d));
      const d = deps.length ? Math.max(...deps.map((x) => depthOf(x, seen))) + 1 : 0;
      depth[id] = d;
      return d;
    };
    ids.forEach((id) => depthOf(id));
    const columns = {};
    for (const id of ids) (columns[depth[id]] ||= []).push(id);
    Object.entries(columns).forEach(([d, col]) => {
      col.forEach((id, i) => { layout[id] = { x: 60 + d * 290, y: 60 + i * 170 }; });
    });
  }

  // ── Canvas rendering ───────────────────────────────────────────
  function applyTransform() {
    canvas.style.transform = `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`;
  }

  function renderCanvas() {
    $('fs-canvas-hint').style.display = steps().length ? 'none' : '';
    $('fs-app-name').value = spec.name || '';
    nodesEl.innerHTML = '';
    for (const step of steps()) nodesEl.appendChild(buildNode(step));
    // Edges need node heights — draw after layout.
    requestAnimationFrame(renderEdges);
    applyTransform();
  }

  function buildNode(step) {
    const el = document.createElement('div');
    el.className = 'fs-node';
    el.dataset.id = step.id;
    const pos = layout[step.id] || { x: 0, y: 0 };
    el.style.left = `${pos.x}px`;
    el.style.top = `${pos.y}px`;
    if (step.terminal) el.classList.add('terminal');
    if (step.id === selectedId) el.classList.add('selected');
    if (liveStatus) {
      if (liveStatus.done?.includes(step.id)) el.classList.add('flow-done');
      else if (liveStatus.active?.includes(step.id)) el.classList.add('flow-active');
    }

    const isStart = !(step.after || []).length;
    let tag = '';
    if (liveStatus?.done?.includes(step.id)) tag = '<span class="fs-node-tag done-live">done</span>';
    else if (liveStatus?.active?.includes(step.id)) tag = '<span class="fs-node-tag active-live">active</span>';
    else if (step.terminal) tag = '<span class="fs-node-tag terminal">terminal</span>';
    else if (isStart) tag = '<span class="fs-node-tag start">start</span>';

    const tools = (step.allow || []).map((t) => `<span class="fs-node-tool">${esc(t)}</span>`).join('')
      + (step.deny || []).map((t) => `<span class="fs-node-tool denied">${esc(t)}</span>`).join('');

    el.innerHTML = `
      <div class="fs-node-head"><span class="fs-node-id">${esc(step.id)}</span>${tag}</div>
      <div class="fs-node-body">
        ${step.posture ? `<div class="fs-node-posture">${esc(step.posture)}</div>` : ''}
        ${step.done ? `<div class="fs-node-guard"><span class="fs-k">done</span>${esc(guardSummary(step.done))}</div>` : ''}
        ${tools ? `<div class="fs-node-tools">${tools}</div>` : ''}
      </div>
      <div class="fs-port" title="Drag to another step: it will run after this one"></div>
      <div class="fs-port fs-port-in" title="Dependencies arrive here"></div>`;

    el.addEventListener('mousedown', (e) => {
      if (e.target.classList.contains('fs-port') && !e.target.classList.contains('fs-port-in')) {
        startEdgeDrag(step.id, e);
      } else if (e.target.closest('.fs-node-head')) {
        startNodeDrag(step.id, el, e);
      }
      e.stopPropagation();
    });
    el.addEventListener('click', (e) => { selectStep(step.id); e.stopPropagation(); });
    return el;
  }

  function nodeAnchor(id, side) {
    const el = nodesEl.querySelector(`[data-id="${CSS.escape(id)}"]`);
    const pos = layout[id] || { x: 0, y: 0 };
    const h = el ? el.offsetHeight : 80;
    const w = el ? el.offsetWidth : 220;
    return side === 'out'
      ? { x: pos.x + w, y: pos.y + h / 2 }
      : { x: pos.x, y: pos.y + h / 2 };
  }

  function edgePath(a, b) {
    const dx = Math.max(40, Math.abs(b.x - a.x) / 2);
    return `M ${a.x} ${a.y} C ${a.x + dx} ${a.y}, ${b.x - dx} ${b.y}, ${b.x} ${b.y}`;
  }

  function renderEdges() {
    let svg = '<defs><marker id="fs-arrow" viewBox="0 0 10 10" refX="9" refY="5" '
      + 'markerWidth="6" markerHeight="6" orient="auto-start-reverse">'
      + '<path d="M 0 0 L 10 5 L 0 10 z" class="fs-edge-arrow"/></marker></defs>';
    for (const step of steps()) {
      for (const dep of step.after || []) {
        const from = depStep(dep);
        if (!findStep(from)) continue;
        const when = depWhen(dep);
        const a = nodeAnchor(from, 'out');
        const b = nodeAnchor(step.id, 'in');
        const cls = when ? 'fs-edge conditional' : 'fs-edge';
        const title = when
          ? `${from} → ${step.id} when ${guardSummary(when)} (click to edit)`
          : `${from} → ${step.id} (click to edit)`;
        svg += `<path class="${cls}" data-from="${esc(from)}" data-to="${esc(step.id)}" `
          + `d="${edgePath(a, b)}" marker-end="url(#fs-arrow)"><title>${esc(title)}</title></path>`;
        if (when) {
          const mx = 0.125 * a.x + 0.375 * (a.x + Math.max(40, Math.abs(b.x - a.x) / 2))
            + 0.375 * (b.x - Math.max(40, Math.abs(b.x - a.x) / 2)) + 0.125 * b.x;
          const my = 0.125 * a.y + 0.375 * a.y + 0.375 * b.y + 0.125 * b.y;
          svg += `<text class="fs-edge-label" x="${mx}" y="${my - 6}" text-anchor="middle">`
            + `${esc(truncate(guardSummary(when), 34))}</text>`;
        }
      }
    }
    svg += '<path id="fs-ghost-edge" class="fs-edge-ghost" d="" style="display:none"/>';
    edgesSvg.innerHTML = svg;
    edgesSvg.querySelectorAll('.fs-edge').forEach((p) => {
      p.addEventListener('click', (e) => {
        // Shift+click removes; plain click opens the target step's edge list.
        const to = findStep(p.dataset.to);
        if (!to) return;
        if (e.shiftKey) {
          to.after = to.after.filter((d) => depStep(d) !== p.dataset.from);
          commit();
          if (selectedId === p.dataset.to) renderStepForm();
        } else {
          selectStep(p.dataset.to);
        }
        e.stopPropagation();
      });
    });
  }

  // ── Node dragging ──────────────────────────────────────────────
  function canvasPoint(e) {
    const r = canvasWrap.getBoundingClientRect();
    return { x: (e.clientX - r.left - pan.x) / zoom, y: (e.clientY - r.top - pan.y) / zoom };
  }

  function startNodeDrag(id, el, e) {
    const start = canvasPoint(e);
    const orig = { ...layout[id] };
    el.classList.add('dragging');
    const move = (ev) => {
      const p = canvasPoint(ev);
      layout[id] = { x: Math.round(orig.x + p.x - start.x), y: Math.round(orig.y + p.y - start.y) };
      el.style.left = `${layout[id].x}px`;
      el.style.top = `${layout[id].y}px`;
      renderEdges();
    };
    const up = () => {
      el.classList.remove('dragging');
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
      persist();
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
  }

  // ── Edge dragging (out-port → node) ────────────────────────────
  function startEdgeDrag(fromId, e) {
    const ghost = () => $('fs-ghost-edge');
    const a = nodeAnchor(fromId, 'out');
    let target = null;
    const move = (ev) => {
      const p = canvasPoint(ev);
      const g = ghost();
      if (g) { g.style.display = ''; g.setAttribute('d', edgePath(a, p)); }
      const el = document.elementFromPoint(ev.clientX, ev.clientY)?.closest('.fs-node');
      nodesEl.querySelectorAll('.drop-target').forEach((n) => n.classList.remove('drop-target'));
      target = null;
      if (el && el.dataset.id !== fromId) {
        el.classList.add('drop-target');
        target = el.dataset.id;
      }
    };
    const up = () => {
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
      nodesEl.querySelectorAll('.drop-target').forEach((n) => n.classList.remove('drop-target'));
      const g = ghost();
      if (g) g.style.display = 'none';
      if (target) {
        const t = findStep(target);
        if (t && !t.after.some((d) => depStep(d) === fromId)) t.after.push(fromId);
        commit();
        if (selectedId === target) renderStepForm();
      }
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
  }

  // ── Canvas pan & zoom ──────────────────────────────────────────
  canvasWrap.addEventListener('mousedown', (e) => {
    if (e.target.closest('.fs-node') || e.target.closest('.fs-zoom')) return;
    canvasWrap.classList.add('panning');
    const sx = e.clientX, sy = e.clientY, ox = pan.x, oy = pan.y;
    const move = (ev) => { pan = { x: ox + ev.clientX - sx, y: oy + ev.clientY - sy }; applyTransform(); };
    const up = () => {
      canvasWrap.classList.remove('panning');
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
  });
  canvasWrap.addEventListener('click', (e) => {
    if (!e.target.closest('.fs-node')) { selectedId = null; renderCanvas(); renderStepForm(); }
  });
  const setZoom = (z) => { zoom = Math.min(2, Math.max(0.35, z)); $('fs-zoom-reset').textContent = `${Math.round(zoom * 100)}%`; applyTransform(); };
  $('fs-zoom-in').addEventListener('click', () => setZoom(zoom + 0.15));
  $('fs-zoom-out').addEventListener('click', () => setZoom(zoom - 0.15));
  $('fs-zoom-reset').addEventListener('click', () => setZoom(1));

  // ── Selection & step form ──────────────────────────────────────
  function selectStep(id) {
    selectedId = id;
    renderCanvas();
    switchTab('step');
    renderStepForm();
  }

  function field(labelText, inputEl, help) {
    const wrap = document.createElement('div');
    wrap.className = 'fs-field';
    const label = document.createElement('label');
    label.textContent = labelText;
    wrap.append(label, inputEl);
    if (help) {
      const h = document.createElement('div');
      h.className = 'fs-help';
      h.textContent = help;
      wrap.append(h);
    }
    return wrap;
  }

  const textInput = (value, oninput, { mono = false, placeholder = '' } = {}) => {
    const i = document.createElement('input');
    i.type = 'text';
    if (mono) i.className = 'fs-mono';
    i.placeholder = placeholder;
    i.value = value ?? '';
    i.addEventListener('input', () => oninput(i.value));
    return i;
  };

  const textArea = (value, oninput, { mono = false, placeholder = '' } = {}) => {
    const t = document.createElement('textarea');
    if (mono) t.className = 'fs-mono';
    t.placeholder = placeholder;
    t.value = value ?? '';
    t.addEventListener('input', () => oninput(t.value));
    return t;
  };

  const csv = (arr) => (arr || []).join(', ');
  const parseCsv = (s) => s.split(',').map((x) => x.trim()).filter(Boolean);

  function renderStepForm() {
    const form = $('fs-step-form');
    const empty = $('fs-step-empty');
    const step = selectedId ? findStep(selectedId) : null;
    form.hidden = !step;
    empty.hidden = !!step;
    form.innerHTML = '';
    if (!step) return;

    form.append(field('Step id', textInput(step.id, (v) => {
      const to = v.trim();
      if (!to || to === step.id || findStep(to)) return;
      renameStep(step.id, to);
    }, { mono: true }), 'Renames propagate to dependencies, constraints, and guards.'));

    const postureInput = textArea(step.posture, (v) => { step.posture = v || null; softCommit(); },
      { placeholder: 'Instruction imposed on the model while this step is active' });
    // Live edit: while a session runs, a committed posture edit steers the
    // very next turn.
    postureInput.addEventListener('change', () => {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: 'updateFlowPostures', postures: { [step.id]: step.posture || '' } }));
        chatMsg('system', `posture of '${step.id}' updated live`);
      }
    });
    form.append(field('Posture', postureInput,
      ws ? 'Session running — edits apply on the next turn.' : undefined));

    const groundInput = textInput(step.ground, (v) => { step.ground = v || null; softCommit(); },
      { mono: true, placeholder: 'e.g. Balance is {balance_usd}.' });
    groundInput.addEventListener('change', () => {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: 'updateFlowPostures', grounds: { [step.id]: step.ground || '' } }));
      }
    });
    form.append(field('Ground template', groundInput,
      'State-interpolated fact line projected while active. {key} or {key?yes:no}.'));

    form.append(field('Allowed tools', textInput(csv(step.allow), (v) => { step.allow = parseCsv(v); softCommit(); },
      { mono: true, placeholder: 'tool_a, tool_b' }),
      'Whitelist while active — leaving it empty means no restriction.'));

    form.append(field('Denied tools', textInput(csv(step.deny), (v) => { step.deny = parseCsv(v); softCommit(); }, { mono: true })));

    const term = document.createElement('label');
    term.className = 'fs-check';
    const cb = document.createElement('input');
    cb.type = 'checkbox';
    cb.checked = !!step.terminal;
    cb.addEventListener('change', () => {
      step.terminal = cb.checked;
      if (cb.checked) step.done = null;
      commit();
      renderStepForm();
    });
    term.append(cb, document.createTextNode(' Terminal step (completes on eligibility, needs no done guard)'));
    form.append(term);

    if (!step.terminal) {
      const doneWrap = document.createElement('div');
      doneWrap.append(guardEditor(step.done, (g) => { step.done = g; softCommit(); }, 'required for non-terminal steps'));
      form.append(field('Done when', doneWrap, 'Completion guard — the step latches done when this holds.'));
    }

    const gateWrap = document.createElement('div');
    gateWrap.append(guardEditor(step.gate, (g) => { step.gate = g; softCommit(); }, 'none'));
    form.append(field('Gate (extra eligibility)', gateWrap, 'Optional guard beyond dependencies.'));

    // Incoming edges: source + optional condition per edge, plus the join mode.
    const edgesWrap = document.createElement('div');
    (step.after || []).forEach((dep, idx) => {
      const row = document.createElement('div');
      row.className = 'fs-card';
      const head = document.createElement('div');
      head.className = 'fs-card-head';
      head.innerHTML = `<span class="fs-card-title">after ${esc(depStep(dep))}</span>`;
      const rm = document.createElement('button');
      rm.className = 'fs-icon-btn';
      rm.textContent = '\u00d7';
      rm.title = 'Remove edge';
      rm.addEventListener('click', () => { step.after.splice(idx, 1); commit(); renderStepForm(); });
      head.append(rm);
      row.append(head);
      const gw = document.createElement('div');
      gw.append(guardEditor(depWhen(dep), (g) => {
        // null → plain string edge; guard → conditional object edge.
        step.after[idx] = g ? { step: depStep(dep), when: g } : depStep(dep);
        softCommit();
      }, 'unconditional'));
      row.append(field('Condition', gw, 'Edge satisfied only while this holds.'));
      edgesWrap.append(row);
    });
    if ((step.after || []).length) {
      const join = document.createElement('select');
      join.innerHTML = '<option value="all">all — every edge must be satisfied</option>'
        + '<option value="any">any — one satisfied edge suffices (merge after a branch)</option>';
      join.value = step.join === 'any' ? 'any' : 'all';
      join.addEventListener('change', () => {
        if (join.value === 'any') step.join = 'any'; else delete step.join;
        softCommit();
      });
      edgesWrap.append(field('Join', join));
    }
    form.append(field('Incoming edges', edgesWrap,
      'Drag between nodes on the canvas to add one; give an edge a condition to branch.'));

    const del = document.createElement('button');
    del.className = 'fs-btn fs-btn-danger';
    del.textContent = 'Delete step';
    del.addEventListener('click', () => deleteStep(step.id));
    form.append(del);
  }

  /** Commit that keeps the currently focused sidebar input alive. */
  function softCommit() {
    normalizeSpec();
    renderCanvas();
    setBadge('idle', 'not validated');
    const jsonText = $('fs-json-text');
    if (document.activeElement !== jsonText) syncJsonPane();
    persist();
  }

  function renameStep(from, to) {
    const step = findStep(from);
    if (!step) return;
    step.id = to;
    for (const s of steps()) {
      s.after = (s.after || []).map((d) => {
        if (typeof d === 'string') return d === from ? to : d;
        return d.step === from ? { ...d, step: to } : d;
      });
    }
    for (const s of steps()) {
      if (s.gate) s.gate = walkGuardRename(s.gate, from, to);
      if (s.done) s.done = walkGuardRename(s.done, from, to);
    }
    spec.flow.constraints = (spec.flow.constraints || []).map((c) => {
      if (c.before) return { before: c.before.map((x) => (x === from ? to : x)) };
      if (c.require) return { require: c.require.map((x) => (x === from ? to : x)) };
      if (c.never_until) return { never_until: { tool: c.never_until.tool, until: walkGuardRename(c.never_until.until, from, to) } };
      return c;
    });
    layout[to] = layout[from];
    delete layout[from];
    if (selectedId === from) selectedId = to;
    softCommit();
  }

  function deleteStep(id) {
    spec.flow.steps = steps().filter((s) => s.id !== id);
    for (const s of steps()) s.after = (s.after || []).filter((d) => depStep(d) !== id);
    delete layout[id];
    if (selectedId === id) selectedId = null;
    commit();
    renderStepForm();
  }

  // ── Guard editor (recursive) ───────────────────────────────────
  const GUARD_KINDS = ['none', 'always', 'is_true', 'is_set', 'eq', 'captured', 'called_ok', 'done', 'all', 'any', 'not'];

  function guardKind(g) {
    if (g == null) return 'none';
    if (g === 'always') return 'always';
    return Object.keys(g)[0] || 'none';
  }

  function defaultGuard(kind) {
    switch (kind) {
      case 'none': return null;
      case 'always': return 'always';
      case 'is_true': case 'is_set': case 'called_ok': case 'done': return { [kind]: '' };
      case 'eq': return { eq: ['', true] };
      case 'captured': return { captured: [] };
      case 'all': case 'any': return { [kind]: [{ is_true: '' }] };
      case 'not': return { not: { is_true: '' } };
      default: return null;
    }
  }

  function guardEditor(guard, onChange, nonePlaceholder = 'none') {
    const root = document.createElement('div');
    root.className = 'fs-guard';

    const render = (g) => {
      root.innerHTML = '';
      const kind = guardKind(g);
      const sel = document.createElement('select');
      for (const k of GUARD_KINDS) {
        const o = document.createElement('option');
        o.value = k;
        o.textContent = k === 'none' ? `(${nonePlaceholder})` : k;
        if (k === kind) o.selected = true;
        sel.append(o);
      }
      sel.addEventListener('change', () => {
        const g2 = defaultGuard(sel.value);
        onChange(g2);
        render(g2);
      });
      root.append(sel);
      if (g == null || g === 'always') return;

      const [k, v] = Object.entries(g)[0];
      const setArg = (nv) => { g[k] = nv; onChange(g); };

      if (['is_true', 'is_set', 'called_ok', 'done'].includes(k)) {
        const i = document.createElement('input');
        i.placeholder = k === 'called_ok' ? 'tool name' : k === 'done' ? 'step id' : 'state key';
        if (k === 'is_true' || k === 'is_set') i.setAttribute('list', 'fs-state-keys');
        i.value = v;
        i.addEventListener('input', () => setArg(i.value));
        root.append(i);
      } else if (k === 'eq') {
        const key = document.createElement('input');
        key.placeholder = 'state key';
        key.setAttribute('list', 'fs-state-keys');
        key.value = v[0];
        key.addEventListener('input', () => { v[0] = key.value; onChange(g); });
        const val = document.createElement('input');
        val.placeholder = 'JSON value, e.g. "high" or 3';
        val.value = JSON.stringify(v[1]);
        val.addEventListener('input', () => {
          try { v[1] = JSON.parse(val.value); val.style.borderColor = ''; onChange(g); }
          catch (_) { val.style.borderColor = 'var(--error)'; }
        });
        root.append(key, val);
      } else if (k === 'captured') {
        const i = document.createElement('input');
        i.placeholder = 'state keys, comma-separated';
        i.value = v.join(', ');
        i.addEventListener('input', () => setArg(parseCsv(i.value)));
        root.append(i);
      } else if (k === 'all' || k === 'any') {
        const children = document.createElement('div');
        children.className = 'fs-guard-children';
        v.forEach((child, idx) => {
          const row = document.createElement('div');
          row.style.display = 'flex';
          row.style.gap = '4px';
          row.style.alignItems = 'flex-start';
          const ed = guardEditor(child, (ng) => {
            if (ng == null) v.splice(idx, 1); else v[idx] = ng;
            onChange(g);
            if (ng == null) render(g);
          });
          ed.style.flex = '1';
          const rm = document.createElement('button');
          rm.className = 'fs-icon-btn';
          rm.textContent = '\u00d7';
          rm.title = 'Remove clause';
          rm.addEventListener('click', () => { v.splice(idx, 1); onChange(g); render(g); });
          row.append(ed, rm);
          children.append(row);
        });
        const add = document.createElement('button');
        add.className = 'fs-guard-add';
        add.textContent = `+ ${k === 'all' ? 'and' : 'or'} clause`;
        add.addEventListener('click', () => { v.push({ is_true: '' }); onChange(g); render(g); });
        root.append(children, add);
      } else if (k === 'not') {
        root.append(guardEditor(v, (ng) => { g.not = ng ?? { is_true: '' }; onChange(g); }));
      }
    };

    render(guard);
    return root;
  }

  // ── Flow pane (constraints, ambient) ───────────────────────────
  function renderFlowForm() {
    const form = $('fs-flow-form');
    form.innerHTML = '';

    form.append(field('Ambient tools', textInput(csv(spec.flow.ambient), (v) => { spec.flow.ambient = parseCsv(v); softCommit(); },
      { mono: true, placeholder: 'recall_context, escalate' }),
      'Cross-cutting tools exempt from every step’s allow whitelist.'));

    form.append(field('Confirm (commit) tools', textInput(csv(spec.flow.confirm_tools), (v) => {
      const t = parseCsv(v);
      if (t.length) spec.flow.confirm_tools = t; else delete spec.flow.confirm_tools;
      softCommit();
    }, { mono: true }), 'Tools requiring confirmation when reached.'));

    const title = document.createElement('div');
    title.className = 'fs-section-title';
    title.textContent = 'Constraints';
    form.append(title);

    (spec.flow.constraints || []).forEach((c, idx) => form.append(constraintCard(c, idx)));

    const addRow = document.createElement('div');
    addRow.className = 'fs-row';
    const sel = document.createElement('select');
    sel.innerHTML = '<option value="once">once — a tool may run at most once</option>'
      + '<option value="never_until">never…until — forbid a tool until a guard holds</option>'
      + '<option value="before">before — ordering invariant between two steps</option>'
      + '<option value="require">require — steps needed for completion</option>'
      + '<option value="reset">reset — un-latch steps when a guard becomes true (loops)</option>';
    sel.style.flex = '1';
    const add = document.createElement('button');
    add.className = 'fs-btn';
    add.textContent = 'Add';
    add.addEventListener('click', () => {
      const kind = sel.value;
      const fresh = kind === 'once' ? { once: '' }
        : kind === 'never_until' ? { never_until: { tool: '', until: { is_true: '' } } }
        : kind === 'before' ? { before: ['', ''] }
        : kind === 'reset' ? { reset: { steps: [], when: { is_true: '' } } }
        : { require: [] };
      spec.flow.constraints.push(fresh);
      softCommit();
      renderFlowForm();
    });
    addRow.append(sel, add);
    form.append(addRow);

    const ptitle = document.createElement('div');
    ptitle.className = 'fs-section-title';
    ptitle.textContent = 'Temporal patterns';
    form.append(ptitle);
    (spec.patterns || []).forEach((pat, idx) => form.append(patternCard(pat, idx)));
    const addPattern = document.createElement('button');
    addPattern.className = 'fs-btn';
    addPattern.textContent = 'Add pattern';
    addPattern.addEventListener('click', () => {
      spec.patterns.push({ name: `pattern_${spec.patterns.length + 1}`, when: { is_true: '' }, turns: 3, effects: [] });
      softCommit();
      renderFlowForm();
    });
    form.append(addPattern);

    const wtitle = document.createElement('div');
    wtitle.className = 'fs-section-title';
    wtitle.textContent = 'State watchers';
    form.append(wtitle);
    const whelp = document.createElement('div');
    whelp.className = 'fs-help';
    whelp.style.marginBottom = '8px';
    whelp.textContent = 'React the moment a key changes — set state, steer, prompt, or remember.';
    form.append(whelp);
    (spec.watch || []).forEach((w, idx) => form.append(watcherCard(w, idx)));
    const addWatch = document.createElement('button');
    addWatch.className = 'fs-btn';
    addWatch.textContent = 'Add watcher';
    addWatch.addEventListener('click', () => {
      spec.watch.push({ key: '', condition: 'changed', effects: [] });
      softCommit();
      renderFlowForm();
    });
    form.append(addWatch);
  }

  function patternCard(pat, idx) {
    const card = document.createElement('div');
    card.className = 'fs-card';
    const head = document.createElement('div');
    head.className = 'fs-card-head';
    head.innerHTML = `<span class="fs-card-title">${esc(pat.name)}</span>`;
    const rm = document.createElement('button');
    rm.className = 'fs-icon-btn';
    rm.textContent = '\u00d7';
    rm.addEventListener('click', () => { spec.patterns.splice(idx, 1); softCommit(); renderFlowForm(); });
    head.append(rm);
    card.append(head);
    card.append(field('Name', textInput(pat.name, (v) => {
      pat.name = v.trim();
      head.querySelector('.fs-card-title').textContent = pat.name;
      softCommit();
    }, { mono: true })));
    const gw = document.createElement('div');
    gw.append(guardEditor(pat.when, (g) => { pat.when = g ?? { is_true: '' }; softCommit(); }));
    card.append(field('While this holds', gw));
    const mode = document.createElement('select');
    mode.innerHTML = '<option value="turns">for consecutive turns</option>'
      + '<option value="sustained">for sustained seconds</option>';
    mode.value = pat.sustained_secs != null ? 'sustained' : 'turns';
    const amount = textInput(String(pat.sustained_secs ?? pat.turns ?? 3), (v) => {
      const n = parseInt(v, 10);
      if (Number.isNaN(n)) return;
      if (mode.value === 'sustained') { pat.sustained_secs = n; delete pat.turns; }
      else { pat.turns = n; delete pat.sustained_secs; }
      softCommit();
    }, { mono: true });
    mode.addEventListener('change', () => {
      const n = parseInt(amount.value, 10) || 3;
      if (mode.value === 'sustained') { pat.sustained_secs = n; delete pat.turns; }
      else { pat.turns = n; delete pat.sustained_secs; }
      softCommit();
    });
    const row = document.createElement('div');
    row.className = 'fs-row';
    row.append(mode, amount);
    card.append(field('Fires after', row));
    pat.effects = pat.effects || [];
    card.append(field('Effects', effectsEditor(pat.effects, () => softCommit())));
    return card;
  }

  // ── Effects editor (the closed effect vocabulary) ──────────────
  const EFFECT_KINDS = [
    ['set', 'set state'],
    ['context', 'inject context'],
    ['prompt', 'prompt the model'],
    ['remember', 'remember (durable)'],
  ];

  function effectsEditor(effects, onChange) {
    const root = document.createElement('div');
    const render = () => {
      root.innerHTML = '';
      effects.forEach((eff, idx) => {
        const row = document.createElement('div');
        row.className = 'fs-effect';
        const kind = Object.keys(eff)[0] || 'set';
        const sel = document.createElement('select');
        for (const [k, label] of EFFECT_KINDS) {
          const o = document.createElement('option');
          o.value = k;
          o.textContent = label;
          if (k === kind) o.selected = true;
          sel.append(o);
        }
        sel.addEventListener('change', () => {
          effects[idx] = sel.value === 'set' ? { set: {} } : { [sel.value]: '' };
          onChange();
          render();
        });
        const rm = document.createElement('button');
        rm.className = 'fs-icon-btn';
        rm.textContent = '×';
        rm.title = 'Remove effect';
        rm.addEventListener('click', () => { effects.splice(idx, 1); onChange(); render(); });
        const head = document.createElement('div');
        head.className = 'fs-row';
        sel.style.flex = '1';
        head.append(sel, rm);
        row.append(head);
        if (kind === 'set') {
          const t = textArea(Object.keys(eff.set || {}).length ? JSON.stringify(eff.set, null, 2) : '', (v) => {
            const trimmed = v.trim();
            if (!trimmed) { eff.set = {}; onChange(); t.style.borderColor = ''; return; }
            try { eff.set = JSON.parse(trimmed); t.style.borderColor = ''; onChange(); }
            catch (_) { t.style.borderColor = 'var(--error)'; }
          }, { mono: true, placeholder: '{"needs_help": true}' });
          row.append(t);
        } else {
          const placeholder = kind === 'context'
            ? 'Steering text the model reads before its next response'
            : kind === 'prompt'
              ? 'Text the model responds to now — it speaks'
              : 'Durable note; {state.key} interpolates ("caller prefers {state.slot}")';
          row.append(textInput(eff[kind], (v) => { eff[kind] = v; onChange(); }, { placeholder }));
        }
        root.append(row);
      });
      const add = document.createElement('button');
      add.className = 'fs-guard-add';
      add.textContent = '+ effect';
      add.addEventListener('click', () => { effects.push({ set: {} }); onChange(); render(); });
      root.append(add);
    };
    render();
    return root;
  }

  function watcherCard(w, idx) {
    const card = document.createElement('div');
    card.className = 'fs-card';
    const head = document.createElement('div');
    head.className = 'fs-card-head';
    head.innerHTML = `<span class="fs-card-title">${esc(w.key || '(key)')}</span>`;
    const rm = document.createElement('button');
    rm.className = 'fs-icon-btn';
    rm.textContent = '×';
    rm.addEventListener('click', () => { spec.watch.splice(idx, 1); softCommit(); renderFlowForm(); });
    head.append(rm);
    card.append(head);
    const key = textInput(w.key, (v) => {
      w.key = v.trim();
      head.querySelector('.fs-card-title').textContent = w.key || '(key)';
      softCommit();
    }, { mono: true, placeholder: 'state key to observe' });
    key.setAttribute('list', 'fs-state-keys');
    card.append(field('Watched key', key));

    const kinds = [
      ['changed', 'changed (any)'],
      ['changed_to', 'changed to value'],
      ['crossed_above', 'crossed above'],
      ['crossed_below', 'crossed below'],
      ['became_true', 'became true'],
      ['became_false', 'became false'],
    ];
    const kind = typeof w.condition === 'string' ? w.condition : Object.keys(w.condition || {})[0] || 'changed';
    const sel = document.createElement('select');
    for (const [k, label] of kinds) {
      const o = document.createElement('option');
      o.value = k;
      o.textContent = label;
      if (k === kind) o.selected = true;
      sel.append(o);
    }
    const valueInput = textInput(
      typeof w.condition === 'object' && w.condition ? JSON.stringify(Object.values(w.condition)[0]) : '',
      (v) => {
        try {
          const parsed = JSON.parse(v);
          w.condition = { [sel.value]: parsed };
          valueInput.style.borderColor = '';
          softCommit();
        } catch (_) { valueInput.style.borderColor = 'var(--error)'; }
      },
      { mono: true, placeholder: sel.value === 'changed_to' ? 'JSON value, e.g. "done"' : 'number, e.g. 0.9' },
    );
    const applyKind = () => {
      const k = sel.value;
      if (k === 'changed' || k === 'became_true' || k === 'became_false') {
        w.condition = k;
        valueInput.style.display = 'none';
      } else {
        const parsed = (() => { try { return JSON.parse(valueInput.value); } catch (_) { return k === 'changed_to' ? '' : 0; } })();
        w.condition = { [k]: parsed };
        valueInput.style.display = '';
      }
      softCommit();
    };
    sel.addEventListener('change', applyKind);
    if (kind === 'changed' || kind === 'became_true' || kind === 'became_false') valueInput.style.display = 'none';
    const row = document.createElement('div');
    row.className = 'fs-row';
    sel.style.flex = '1';
    valueInput.style.flex = '1';
    row.append(sel, valueInput);
    card.append(field('Condition', row));

    w.effects = w.effects || [];
    card.append(field('Effects', effectsEditor(w.effects, () => softCommit()),
      'Watchers see the live session — they can set state, steer, prompt, or remember.'));
    return card;
  }

  function constraintCard(c, idx) {
    const card = document.createElement('div');
    card.className = 'fs-card';
    const head = document.createElement('div');
    head.className = 'fs-card-head';
    const kind = Object.keys(c)[0];
    head.innerHTML = `<span class="fs-card-title">${esc(kind)}</span>`;
    const rm = document.createElement('button');
    rm.className = 'fs-icon-btn';
    rm.textContent = '\u00d7';
    rm.addEventListener('click', () => { spec.flow.constraints.splice(idx, 1); softCommit(); renderFlowForm(); });
    head.append(rm);
    card.append(head);

    if (kind === 'once') {
      card.append(field('Tool', textInput(c.once, (v) => { c.once = v.trim(); softCommit(); }, { mono: true })));
    } else if (kind === 'before') {
      card.append(field('First step', textInput(c.before[0], (v) => { c.before[0] = v.trim(); softCommit(); }, { mono: true })));
      card.append(field('Must precede', textInput(c.before[1], (v) => { c.before[1] = v.trim(); softCommit(); }, { mono: true })));
    } else if (kind === 'require') {
      card.append(field('Required steps', textInput(csv(c.require), (v) => { c.require = parseCsv(v); softCommit(); }, { mono: true })));
    } else if (kind === 'never_until') {
      card.append(field('Tool', textInput(c.never_until.tool, (v) => { c.never_until.tool = v.trim(); softCommit(); }, { mono: true })));
      const gw = document.createElement('div');
      gw.append(guardEditor(c.never_until.until, (g) => { c.never_until.until = g ?? { is_true: '' }; softCommit(); }));
      card.append(field('Until', gw));
    } else if (kind === 'reset') {
      card.append(field('Steps to un-latch', textInput(csv(c.reset.steps), (v) => { c.reset.steps = parseCsv(v); softCommit(); }, { mono: true })));
      const gw = document.createElement('div');
      gw.append(guardEditor(c.reset.when, (g) => { c.reset.when = g ?? { is_true: '' }; softCommit(); }));
      card.append(field('When (rising edge)', gw,
        'Fires when this becomes true; called_ok evidence for the steps\u2019 done guards is forgiven.'));
    }
    return card;
  }

  // ── App pane (session + tools) ─────────────────────────────────
  function renderAppForm() {
    const form = $('fs-app-form');
    form.innerHTML = '';

    form.append(field('Description', textInput(spec.description, (v) => { spec.description = v; softCommit(); })));
    form.append(field('System instruction', textArea(spec.instruction, (v) => { spec.instruction = v; softCommit(); },
      { placeholder: 'Base persona and rules for the whole session' })));
    form.append(field('Greeting', textArea(spec.greeting, (v) => { spec.greeting = v || null; softCommit(); },
      { placeholder: 'If set, the model speaks first on connect' })));

    const modality = document.createElement('select');
    modality.innerHTML = '<option value="text">text</option><option value="audio">audio (voice)</option>';
    modality.value = spec.modality || 'text';
    modality.addEventListener('change', () => { spec.modality = modality.value; softCommit(); });
    form.append(field('Modality', modality));

    form.append(field('Voice', textInput(spec.voice, (v) => { spec.voice = v || null; softCommit(); },
      { placeholder: 'Puck, Kore, Aoede… (audio only)' })));

    const title = document.createElement('div');
    title.className = 'fs-section-title';
    title.textContent = 'Mock tools';
    form.append(title);
    const help = document.createElement('div');
    help.className = 'fs-help';
    help.style.marginBottom = '8px';
    help.textContent = 'Each tool returns its canned response and writes its set_state keys — '
      + 'so guards latch without writing any code. Swap in real tools later.';
    form.append(help);

    (spec.tools || []).forEach((t, idx) => form.append(toolCard(t, idx)));

    const add = document.createElement('button');
    add.className = 'fs-btn';
    add.textContent = 'Add tool';
    add.addEventListener('click', () => {
      spec.tools.push({ name: `tool_${spec.tools.length + 1}`, description: '', set_state: {} });
      softCommit();
      renderAppForm();
    });
    form.append(add);

    renderStateSection(form);
    renderComputedSection(form);
    renderMemorySection(form);
    renderRuntimeSection(form);
  }

  // ── State dictionary ───────────────────────────────────────────
  function renderStateSection(form) {
    const title = document.createElement('div');
    title.className = 'fs-section-title';
    title.textContent = 'State dictionary';
    form.append(title);
    const help = document.createElement('div');
    help.className = 'fs-help';
    help.style.marginBottom = '8px';
    help.textContent = 'Declare the session’s keys: type, meaning, starting value. '
      + 'Powers autocomplete in every guard editor and typed keys in generated code.';
    form.append(help);

    Object.entries(spec.state || {}).forEach(([key, fieldSpec]) => {
      const card = document.createElement('div');
      card.className = 'fs-card';
      const head = document.createElement('div');
      head.className = 'fs-card-head';
      head.innerHTML = `<span class="fs-card-title">${esc(key)}</span>`;
      const rm = document.createElement('button');
      rm.className = 'fs-icon-btn';
      rm.textContent = '×';
      rm.addEventListener('click', () => { delete spec.state[key]; softCommit(); renderAppForm(); });
      head.append(rm);
      card.append(head);
      card.append(field('Key', textInput(key, (v) => {
        const nk = v.trim();
        if (!nk || nk === key) return;
        spec.state[nk] = spec.state[key];
        delete spec.state[key];
        softCommit();
        renderAppForm();
      }, { mono: true })));
      const type = document.createElement('select');
      type.innerHTML = '<option value="">(untyped)</option>'
        + ['boolean', 'number', 'string', 'object', 'array']
          .map((t) => `<option value="${t}">${t}</option>`).join('');
      type.value = fieldSpec.type || '';
      type.addEventListener('change', () => {
        if (type.value) fieldSpec.type = type.value; else delete fieldSpec.type;
        softCommit();
      });
      card.append(field('Type', type));
      card.append(field('Default', textInput(
        fieldSpec.default === undefined ? '' : JSON.stringify(fieldSpec.default),
        (v) => {
          const trimmed = v.trim();
          if (!trimmed) { delete fieldSpec.default; softCommit(); return; }
          try { fieldSpec.default = JSON.parse(trimmed); softCommit(); } catch (_) { /* keep typing */ }
        }, { mono: true, placeholder: 'JSON, e.g. false or 0' })));
      card.append(field('Description', textInput(fieldSpec.description, (v) => {
        if (v) fieldSpec.description = v; else delete fieldSpec.description;
        softCommit();
      })));
      form.append(card);
    });
    const add = document.createElement('button');
    add.className = 'fs-btn';
    add.textContent = 'Add state key';
    add.addEventListener('click', () => {
      let n = 1;
      while (spec.state[`key_${n}`]) n += 1;
      spec.state[`key_${n}`] = { type: 'boolean' };
      softCommit();
      renderAppForm();
    });
    form.append(add);
  }

  // ── Computed variables ─────────────────────────────────────────
  function renderComputedSection(form) {
    const title = document.createElement('div');
    title.className = 'fs-section-title';
    title.textContent = 'Computed variables';
    form.append(title);
    const help = document.createElement('div');
    help.className = 'fs-help';
    help.style.marginBottom = '8px';
    help.textContent = 'Pure expressions over state, recomputed automatically — guards read the '
      + 'result by its key. Atoms: key, const, add, mul, sub, div, min, max, eq, gt, gte, lt, '
      + 'lte, all, any, not, if, coalesce, concat, count_true.';
    form.append(help);

    (spec.computed || []).forEach((c, idx) => {
      const card = document.createElement('div');
      card.className = 'fs-card';
      const head = document.createElement('div');
      head.className = 'fs-card-head';
      head.innerHTML = `<span class="fs-card-title">${esc(c.key || '(key)')}</span>`;
      const rm = document.createElement('button');
      rm.className = 'fs-icon-btn';
      rm.textContent = '×';
      rm.addEventListener('click', () => { spec.computed.splice(idx, 1); softCommit(); renderAppForm(); });
      head.append(rm);
      card.append(head);
      card.append(field('Key', textInput(c.key, (v) => {
        c.key = v.trim();
        head.querySelector('.fs-card-title').textContent = c.key || '(key)';
        softCommit();
      }, { mono: true })));
      card.append(jsonField('Expression', c.from,
        (v) => { c.from = v || { key: '' }; softCommit(); },
        '{"gt": [{"key": "score"}, {"const": 0.5}]}'));
      card.append(field('Description', textInput(c.description, (v) => {
        if (v) c.description = v; else delete c.description;
        softCommit();
      })));
      form.append(card);
    });
    const add = document.createElement('button');
    add.className = 'fs-btn';
    add.textContent = 'Add computed variable';
    add.addEventListener('click', () => {
      spec.computed.push({ key: `derived_${spec.computed.length + 1}`, from: { key: '' } });
      softCommit();
      renderAppForm();
    });
    form.append(add);
  }

  // ── Memory ─────────────────────────────────────────────────────
  function renderMemorySection(form) {
    const title = document.createElement('div');
    title.className = 'fs-section-title';
    title.textContent = 'Durable memory';
    form.append(title);

    const toggleRow = document.createElement('div');
    toggleRow.className = 'fs-row';
    const toggle = document.createElement('input');
    toggle.type = 'checkbox';
    toggle.checked = !!spec.memory;
    toggle.addEventListener('change', () => {
      if (toggle.checked) spec.memory = spec.memory || { slots: [] };
      else delete spec.memory;
      softCommit();
      renderAppForm();
    });
    const toggleLabel = document.createElement('label');
    toggleLabel.className = 'fs-help';
    toggleLabel.textContent = 'Remember across sessions — installs the ambient recall_context / '
      + 'manage_memory tools, turn ingestion, and the `remember` effect.';
    toggleRow.append(toggle, toggleLabel);
    form.append(toggleRow);

    if (!spec.memory) return;
    spec.memory.slots = spec.memory.slots || [];
    const help = document.createElement('div');
    help.className = 'fs-help';
    help.style.margin = '8px 0';
    help.textContent = 'Slots project remembered facts into state keys, where needs and guards '
      + 'read them — a returning caller isn’t asked twice.';
    form.append(help);
    spec.memory.slots.forEach((slot, idx) => {
      const row = document.createElement('div');
      row.className = 'fs-row';
      const pred = textInput(slot.predicate, (v) => { slot.predicate = v.trim(); softCommit(); },
        { mono: true, placeholder: 'predicate (dietary_identity)' });
      const to = textInput(slot.to, (v) => { slot.to = v.trim(); softCommit(); },
        { mono: true, placeholder: 'state key (user:diet)' });
      to.setAttribute('list', 'fs-state-keys');
      const rm = document.createElement('button');
      rm.className = 'fs-icon-btn';
      rm.textContent = '×';
      rm.addEventListener('click', () => { spec.memory.slots.splice(idx, 1); softCommit(); renderAppForm(); });
      pred.style.flex = '1';
      to.style.flex = '1';
      row.append(pred, to, rm);
      form.append(row);
    });
    const add = document.createElement('button');
    add.className = 'fs-btn';
    add.textContent = 'Add memory slot';
    add.addEventListener('click', () => {
      spec.memory.slots.push({ predicate: '', to: '' });
      softCommit();
      renderAppForm();
    });
    form.append(add);
  }

  // ── Runtime tuning ─────────────────────────────────────────────
  function renderRuntimeSection(form) {
    const title = document.createElement('div');
    title.className = 'fs-section-title';
    title.textContent = 'Runtime tuning';
    form.append(title);
    const rt = () => { spec.runtime = spec.runtime || {}; return spec.runtime; };
    const setOrClear = (key, value) => {
      const r = rt();
      if (value === undefined || value === '' || value === null || Number.isNaN(value)) delete r[key];
      else r[key] = value;
      if (!Object.keys(r).length) delete spec.runtime;
      softCommit();
    };
    const r = spec.runtime || {};

    const numField = (label, key, placeholder, parse = parseFloat, help) => {
      form.append(field(label, textInput(r[key] != null ? String(r[key]) : '', (v) => {
        setOrClear(key, v.trim() === '' ? undefined : parse(v));
      }, { mono: true, placeholder }), help));
    };
    numField('Temperature', 'temperature', 'model default');
    numField('Thinking budget (tokens)', 'thinking_budget', 'off', (v) => parseInt(v, 10));
    numField('Soft-turn timeout (ms)', 'soft_turn_timeout_ms', 'off', (v) => parseInt(v, 10),
      'Run extractors/watchers when the model chooses silence this long after the caller stops.');

    const selField = (label, key, options, help) => {
      const sel = document.createElement('select');
      sel.innerHTML = '<option value="">(default)</option>'
        + options.map((o) => `<option value="${o}">${o.replaceAll('_', ' ')}</option>`).join('');
      sel.value = r[key] || '';
      sel.addEventListener('change', () => setOrClear(key, sel.value || undefined));
      form.append(field(label, sel, help));
    };
    selField('Steering mode', 'steering', ['instruction_update', 'context_injection', 'hybrid'],
      'context_injection avoids instruction re-processing spikes on phase transitions.');
    selField('Context delivery', 'context_delivery', ['immediate', 'deferred'],
      'deferred queues steering until the next user send — no isolated frames mid-silence.');

    const boolField = (label, key) => {
      const cb = document.createElement('input');
      cb.type = 'checkbox';
      cb.checked = r[key] === true;
      cb.addEventListener('change', () => setOrClear(key, cb.checked ? true : undefined));
      form.append(field(label, cb));
    };
    boolField('Proactive audio (model may stay silent)', 'proactive_audio');
    boolField('Include thought summaries', 'include_thoughts');
    boolField('Lossy audio delivery', 'lossy_audio');

    // VAD sensitivities.
    const vadRow = document.createElement('div');
    vadRow.className = 'fs-row';
    const vadSel = (key, label) => {
      const sel = document.createElement('select');
      sel.innerHTML = `<option value="">${label}: default</option>`
        + ['low', 'medium', 'high'].map((o) => `<option value="${o}">${label}: ${o}</option>`).join('');
      sel.value = (r.vad || {})[key] || '';
      sel.style.flex = '1';
      sel.addEventListener('change', () => {
        const vad = { ...(rt().vad || {}) };
        if (sel.value) vad[key] = sel.value; else delete vad[key];
        setOrClear('vad', Object.keys(vad).length ? vad : undefined);
      });
      return sel;
    };
    vadRow.append(vadSel('start_sensitivity', 'speech start'), vadSel('end_sensitivity', 'speech end'));
    form.append(field('Voice activity detection', vadRow));
    const silence = textInput(
      (r.vad || {}).silence_duration_ms != null ? String(r.vad.silence_duration_ms) : '',
      (v) => {
        const n = parseInt(v, 10);
        const vad = { ...(rt().vad || {}) };
        if (Number.isNaN(n)) delete vad.silence_duration_ms; else vad.silence_duration_ms = n;
        setOrClear('vad', Object.keys(vad).length ? vad : undefined);
      },
      { mono: true, placeholder: 'default' },
    );
    form.append(field('VAD silence before end-of-speech (ms)', silence));

    // Audio hardening: the measured mic chain + client VAD + authority.
    const audioTitle = document.createElement('div');
    audioTitle.className = 'fs-section-title';
    audioTitle.textContent = 'Audio hardening';
    form.append(audioTitle);
    const audio = () => (r.audio || {});
    const audioSet = (key, value) => {
      const a = { ...(rt().audio || {}) };
      if (value === undefined || value === null || value === '' || (typeof value === 'number' && Number.isNaN(value))) delete a[key];
      else a[key] = value;
      setOrClear('audio', Object.keys(a).length ? a : undefined);
    };

    const denoiseCb = document.createElement('input');
    denoiseCb.type = 'checkbox';
    denoiseCb.checked = audio().denoise === true;
    denoiseCb.addEventListener('change', () => audioSet('denoise', denoiseCb.checked ? true : undefined));
    form.append(field('Denoise mic audio (RNNoise)', denoiseCb,
      'Speech enhancer over incoming user audio. Measured: takes the energy VAD from latched-open in street noise to 0 false activations at 0 dB SNR. +10 ms latency.'));

    const gateRow = document.createElement('div');
    gateRow.className = 'fs-row';
    const gateThr = textInput(audio().noise_gate ? String(audio().noise_gate.threshold_rms) : '', (v) => {
      const n = parseFloat(v);
      if (Number.isNaN(n)) { audioSet('noise_gate', undefined); return; }
      audioSet('noise_gate', { threshold_rms: n, hold_frames: audio().noise_gate?.hold_frames ?? 3 });
    }, { mono: true, placeholder: 'gate RMS (400–700; off)' });
    const gateHold = textInput(audio().noise_gate ? String(audio().noise_gate.hold_frames) : '', (v) => {
      const n = parseInt(v, 10);
      if (Number.isNaN(n) || !audio().noise_gate) return;
      audioSet('noise_gate', { threshold_rms: audio().noise_gate.threshold_rms, hold_frames: n });
    }, { mono: true, placeholder: 'hold frames (3)' });
    gateThr.style.flex = '1';
    gateHold.style.flex = '1';
    gateRow.append(gateThr, gateHold);
    form.append(field('Noise gate (after denoiser)', gateRow,
      'Silences frames below the RMS threshold — near-talker preference and horn-residue rejection. Chain behind denoise.'));

    const presetSel = document.createElement('select');
    presetSel.innerHTML = '<option value="">client VAD: default</option>'
      + '<option value="noisy_street">client VAD: noisy street (tuned)</option>';
    presetSel.value = audio().client_vad?.preset || '';
    presetSel.addEventListener('change', () => {
      const cv = { ...(audio().client_vad || {}) };
      if (presetSel.value) cv.preset = presetSel.value; else delete cv.preset;
      audioSet('client_vad', Object.keys(cv).length ? cv : undefined);
    });
    form.append(field('Client VAD preset', presetSel,
      'noisy_street: 21 dB start / 1-frame confirm — the closed-loop-tuned profile for denoised streams (0 false activations at 0 dB traffic).'));
    const cvRow = document.createElement('div');
    cvRow.className = 'fs-row';
    const cvNum = (key, placeholder, parse = parseFloat) => {
      const input = textInput(audio().client_vad?.[key] != null ? String(audio().client_vad[key]) : '', (v) => {
        const n = parse(v);
        const cv = { ...(audio().client_vad || {}) };
        if (Number.isNaN(n)) delete cv[key]; else cv[key] = n;
        audioSet('client_vad', Object.keys(cv).length ? cv : undefined);
      }, { mono: true, placeholder });
      input.style.flex = '1';
      return input;
    };
    cvRow.append(
      cvNum('start_threshold_db', 'start dB'),
      cvNum('min_speech_frames', 'confirm frames', (v) => parseInt(v, 10)),
      cvNum('hangover_frames', 'hangover frames', (v) => parseInt(v, 10)),
    );
    form.append(field('Client VAD overrides', cvRow,
      'Each confirm/hangover frame is 30 ms. Overrides apply on top of the preset.'));

    const authSel = document.createElement('select');
    authSel.innerHTML = '<option value="">interruptions: server decides (default)</option>'
      + '<option value="client">interruptions: this client\u2019s VAD decides</option>';
    authSel.value = audio().authority || '';
    authSel.addEventListener('change', () => audioSet('authority', authSel.value || undefined));
    form.append(field('Interruption authority', authSel,
      'client: ~2× faster barge-in (measured ~400 ms vs ~800 ms) via activity marks; requires denoise (+ gate) or noise falsely interrupts. server: zero false interruptions in every benchmark run.'));

    // Turn-commit tuning.
    const tcRow = document.createElement('div');
    tcRow.className = 'fs-row';
    const eotHold = textInput(audio().eot_hold_ms != null ? String(audio().eot_hold_ms) : '', (v) => {
      const n = parseInt(v, 10);
      if (Number.isNaN(n)) { audioSet('eot_hold_ms', undefined); return; }
      audioSet('eot_hold_ms', n);
    }, { mono: true, placeholder: 'eot_hold_ms (off)' });
    const minInt = textInput(audio().min_interruption_ms != null ? String(audio().min_interruption_ms) : '', (v) => {
      const n = parseInt(v, 10);
      if (Number.isNaN(n)) { audioSet('min_interruption_ms', undefined); return; }
      audioSet('min_interruption_ms', n);
    }, { mono: true, placeholder: 'min_interruption_ms (off)' });
    eotHold.style.flex = '1';
    minInt.style.flex = '1';
    tcRow.append(eotHold, minInt);
    form.append(field('Turn-commit tuning', tcRow,
      'eot_hold_ms: suppress false end-of-turn commits during mid-turn pauses (800 ms = TurnBench 0.1-fp qualifying point, 1600 ms = frontier). min_interruption_ms: suppress false barge-ins on backchannels (1400 ms: fp 0.702 → 0.062, 2000 ms = max window).'));

    // Repair thresholds.
    const repairRow = document.createElement('div');
    repairRow.className = 'fs-row';
    const nudge = textInput(r.repair ? String(r.repair.nudge_after) : '', (v) => {
      const n = parseInt(v, 10);
      if (Number.isNaN(n)) { setOrClear('repair', undefined); return; }
      setOrClear('repair', { nudge_after: n, escalate_after: r.repair?.escalate_after ?? n * 2 });
    }, { mono: true, placeholder: 'nudge after N turns' });
    const escalate = textInput(r.repair ? String(r.repair.escalate_after) : '', (v) => {
      const n = parseInt(v, 10);
      if (Number.isNaN(n) || !rt().repair) return;
      setOrClear('repair', { nudge_after: rt().repair.nudge_after, escalate_after: n });
    }, { mono: true, placeholder: 'escalate after N turns' });
    nudge.style.flex = '1';
    escalate.style.flex = '1';
    repairRow.append(nudge, escalate);
    form.append(field('Conversation repair', repairRow,
      'Nudge, then escalate, when a phase’s needed keys stay unfilled.'));

    // Persistence.
    const persistRow = document.createElement('div');
    persistRow.className = 'fs-row';
    const backend = document.createElement('select');
    backend.innerHTML = '<option value="">no persistence</option>'
      + '<option value="memory">in-memory</option><option value="fs">filesystem</option>';
    backend.value = r.persistence ? (r.persistence === 'memory' ? 'memory' : 'fs') : '';
    const dir = textInput(r.persistence && r.persistence.fs ? r.persistence.fs.dir : '', (v) => {
      if (backend.value === 'fs') setOrClear('persistence', { fs: { dir: v } });
    }, { mono: true, placeholder: '/var/sessions' });
    dir.style.display = backend.value === 'fs' ? '' : 'none';
    backend.addEventListener('change', () => {
      dir.style.display = backend.value === 'fs' ? '' : 'none';
      setOrClear('persistence', backend.value === '' ? undefined
        : backend.value === 'memory' ? 'memory' : { fs: { dir: dir.value || '/tmp/sessions' } });
    });
    backend.style.flex = '1';
    dir.style.flex = '1';
    persistRow.append(backend, dir);
    form.append(field('Session persistence', persistRow));
    form.append(field('Session id', textInput(r.session_id, (v) => setOrClear('session_id', v.trim() || undefined),
      { mono: true, placeholder: 'stable id for resume' })));
  }

  function jsonField(labelText, value, apply, placeholder) {
    const t = textArea(value === undefined ? '' : JSON.stringify(value, null, 2), (v) => {
      const trimmed = v.trim();
      if (!trimmed) { apply(undefined); t.style.borderColor = ''; return; }
      try { apply(JSON.parse(trimmed)); t.style.borderColor = ''; }
      catch (_) { t.style.borderColor = 'var(--error)'; }
    }, { mono: true, placeholder });
    return field(labelText, t);
  }

  function toolCard(t, idx) {
    const card = document.createElement('div');
    card.className = 'fs-card';
    const head = document.createElement('div');
    head.className = 'fs-card-head';
    head.innerHTML = `<span class="fs-card-title">${esc(t.name || '(unnamed)')}</span>`;
    const rm = document.createElement('button');
    rm.className = 'fs-icon-btn';
    rm.textContent = '\u00d7';
    rm.addEventListener('click', () => { spec.tools.splice(idx, 1); softCommit(); renderAppForm(); });
    head.append(rm);
    card.append(head);

    card.append(field('Name', textInput(t.name, (v) => {
      t.name = v.trim();
      head.querySelector('.fs-card-title').textContent = t.name || '(unnamed)';
      softCommit();
    }, { mono: true })));
    card.append(field('Description', textInput(t.description, (v) => { t.description = v; softCommit(); })));
    card.append(jsonField('Parameters (JSON Schema)', t.parameters,
      (v) => { if (v === undefined) delete t.parameters; else t.parameters = v; softCommit(); },
      '{"type":"object","properties":{...}}'));
    card.append(jsonField('Response', t.response,
      (v) => { if (v === undefined) delete t.response; else t.response = v; softCommit(); },
      '{"ok": true}'));
    card.append(jsonField('Set state', t.set_state && Object.keys(t.set_state).length ? t.set_state : undefined,
      (v) => { t.set_state = v || {}; softCommit(); },
      '{"identity_verified": true}'));
    return card;
  }

  // ── JSON pane ──────────────────────────────────────────────────
  function exportSpec() {
    // Strip nulls/empties for a clean document.
    const clean = JSON.parse(JSON.stringify(spec, (k, v) => (v === null ? undefined : v)));
    if (clean.flow) {
      if (!clean.flow.constraints?.length) delete clean.flow.constraints;
      if (!clean.flow.ambient?.length) delete clean.flow.ambient;
      for (const s of clean.flow.steps || []) {
        if (!s.after?.length) delete s.after;
        if (!s.allow?.length) delete s.allow;
        if (!s.deny?.length) delete s.deny;
        if (!s.terminal) delete s.terminal;
      }
    }
    if (!clean.tools?.length) delete clean.tools;
    for (const t of clean.tools || []) {
      if (t.set_state && !Object.keys(t.set_state).length) delete t.set_state;
      if (t.description === '') delete t.description;
    }
    if (!clean.description) delete clean.description;
    if (clean.state && !Object.keys(clean.state).length) delete clean.state;
    if (!clean.computed?.length) delete clean.computed;
    if (!clean.watch?.length) delete clean.watch;
    for (const w of clean.watch || []) {
      if (w.set && !Object.keys(w.set).length) delete w.set;
      if (!w.effects?.length) delete w.effects;
    }
    for (const p of clean.patterns || []) {
      if (!p.effects?.length) delete p.effects;
    }
    if (clean.memory && !clean.memory.slots?.length) clean.memory = {};
    if (clean.runtime && !Object.keys(clean.runtime).length) delete clean.runtime;
    return clean;
  }

  function syncJsonPane() {
    $('fs-json-text').value = JSON.stringify(exportSpec(), null, 2);
    $('fs-json-status').textContent = '';
    $('fs-json-status').className = 'fs-json-status';
  }

  function applyJson(text, { fromImport = false } = {}) {
    const status = $('fs-json-status');
    try {
      const parsed = JSON.parse(text);
      // Accept a bare flow document too, mirroring the server.
      spec = parsed.flow === undefined && parsed.steps !== undefined
        ? { ...blankSpec(), flow: parsed }
        : { ...blankSpec(), ...parsed };
      normalizeSpec();
      selectedId = null;
      liveStatus = null;
      commit({ relayout: true });
      renderStepForm();
      renderFlowForm();
      renderAppForm();
      status.textContent = fromImport ? 'Imported.' : 'Applied.';
      status.className = 'fs-json-status ok';
    } catch (err) {
      status.textContent = `Invalid JSON: ${err.message}`;
      status.className = 'fs-json-status err';
    }
  }

  $('fs-json-apply').addEventListener('click', () => applyJson($('fs-json-text').value));
  $('fs-json-copy').addEventListener('click', async () => {
    syncJsonPane();
    try { await navigator.clipboard.writeText($('fs-json-text').value); } catch (_) { /* no-op */ }
  });
  $('fs-json-download').addEventListener('click', () => {
    syncJsonPane();
    const blob = new Blob([$('fs-json-text').value], { type: 'application/json' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = `${spec.name || 'flow'}.json`;
    a.click();
    URL.revokeObjectURL(a.href);
  });
  $('fs-json-import').addEventListener('change', async (e) => {
    const file = e.target.files?.[0];
    if (file) applyJson(await file.text(), { fromImport: true });
    e.target.value = '';
  });

  // ── Validation ─────────────────────────────────────────────────
  function setBadge(kind, text) {
    const b = $('fs-validation-badge');
    b.className = `fs-badge fs-badge-${kind}`;
    b.textContent = text;
  }

  async function validate({ silent = false } = {}) {
    try {
      const res = await fetch('/api/flows/validate', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(exportSpec()),
      });
      const v = await res.json();
      const items = [
        ...(v.errors || []).map((m) => ({ kind: 'err', m })),
        ...(v.warnings || []).map((m) => ({ kind: 'warn', m })),
      ];
      const plural = (n, w) => `${n} ${w}${n === 1 ? '' : 's'}`;
      if (v.valid) setBadge(items.length ? 'warn' : 'ok', items.length ? `valid · ${plural(items.length, 'warning')}` : 'valid');
      else setBadge('err', plural((v.errors || []).length, 'error'));
      if (!silent) {
        showDiagnostics(v.valid
          ? (items.length ? items : [{ kind: 'ok', m: `Flow compiles — ${plural(v.steps, 'step')}; tools: ${(v.tools || []).join(', ') || 'none'}.` }])
          : items);
      }
      return v;
    } catch (err) {
      setBadge('err', 'validate failed');
      if (!silent) showDiagnostics([{ kind: 'err', m: `Could not reach /api/flows/validate: ${err.message}` }]);
      return { valid: false, errors: [String(err)] };
    }
  }

  function showDiagnostics(items) {
    const list = $('fs-diagnostics-list');
    list.innerHTML = '';
    for (const { kind, m } of items) {
      const li = document.createElement('li');
      li.className = kind;
      li.textContent = m;
      list.append(li);
    }
    $('fs-diagnostics').hidden = false;
  }
  $('fs-diagnostics-close').addEventListener('click', () => { $('fs-diagnostics').hidden = true; });
  $('fs-validation-badge').addEventListener('click', () => validate());
  $('fs-validate-btn').addEventListener('click', () => validate());

  // ── Embedded tests (offline replay through the real flow monitor) ──
  async function runSpecTests() {
    const testCount = (spec.tests || []).length;
    if (!testCount) {
      showDiagnostics([{
        kind: 'warn',
        m: 'No tests in this spec. Add a "tests" array (JSON tab): scripted tool/set/expect '
          + 'events replayed offline through the real flow monitor — no API key needed.',
      }]);
      return;
    }
    try {
      const res = await fetch('/api/flows/test', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(exportSpec()),
      });
      const result = await res.json();
      if (!result.valid) {
        showDiagnostics((result.errors || []).map((m) => ({ kind: 'err', m })));
        return;
      }
      const items = [];
      for (const report of result.reports || []) {
        if (report.passed) {
          items.push({ kind: 'ok', m: `${report.name} — passed (${report.events} events)` });
        } else {
          items.push({ kind: 'err', m: `${report.name} — failed` });
          for (const step of report.failures || []) {
            for (const failure of step.failures || []) {
              items.push({ kind: 'err', m: `  at event ${step.index} (${step.event}): ${failure}` });
            }
          }
        }
      }
      showDiagnostics(items);
    } catch (err) {
      showDiagnostics([{ kind: 'err', m: `Could not reach /api/flows/test: ${err.message}` }]);
    }
  }
  $('fs-tests-btn').addEventListener('click', runSpecTests);

  // ── Run (live session over /ws/flow-studio) ────────────────────
  function chatMsg(cls, text) {
    const el = document.createElement('div');
    el.className = `fs-msg ${cls}`;
    el.textContent = text;
    $('fs-chat').append(el);
    $('fs-chat').scrollTop = $('fs-chat').scrollHeight;
    return el;
  }

  let currentModelMsg = null;

  function setRunConnected(connected) {
    $('fs-chat-text').disabled = !connected;
    $('fs-chat-send').disabled = !connected;
    const runBtn = $('fs-run-btn');
    runBtn.textContent = connected ? 'Stop' : 'Run';
    runBtn.classList.toggle('running', connected);
    $('fs-run-flowstate').hidden = !connected && !liveStatus;
  }

  async function startRun() {
    const v = await validate({ silent: true });
    if (!v.valid) { switchTab('run'); validate(); return; }
    switchTab('run');
    $('fs-chat').innerHTML = '';
    liveStatus = null;
    currentModelMsg = null;
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    ws = new WebSocket(`${proto}://${location.host}/ws/flow-studio`);
    const runStatus = $('fs-run-status');
    runStatus.textContent = 'Connecting…';
    runStatus.className = 'fs-run-status';

    ws.addEventListener('open', () => {
      ws.send(JSON.stringify({ type: 'start', config: exportSpec() }));
    });
    ws.addEventListener('message', (e) => {
      if (typeof e.data !== 'string') return; // binary audio — text mode ignores it
      let msg;
      try { msg = JSON.parse(e.data); } catch (_) { return; }
      handleServerMessage(msg);
    });
    ws.addEventListener('close', () => {
      runStatus.textContent = 'Session ended.';
      runStatus.className = 'fs-run-status';
      ws = null;
      setRunConnected(false);
    });
    ws.addEventListener('error', () => {
      runStatus.textContent = 'Connection error.';
      runStatus.className = 'fs-run-status error';
    });
  }

  function stopRun() {
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: 'stop' }));
    ws?.close();
    ws = null;
    setRunConnected(false);
  }

  function handleServerMessage(msg) {
    const runStatus = $('fs-run-status');
    switch (msg.type) {
      case 'connected':
        runStatus.textContent = 'Connected — governed session live.';
        runStatus.className = 'fs-run-status connected';
        setRunConnected(true);
        break;
      case 'textDelta':
        if (!currentModelMsg) currentModelMsg = chatMsg('model', '');
        currentModelMsg.textContent += msg.text;
        $('fs-chat').scrollTop = $('fs-chat').scrollHeight;
        break;
      case 'textComplete':
        if (!currentModelMsg) currentModelMsg = chatMsg('model', '');
        currentModelMsg.textContent = msg.text;
        break;
      case 'turnComplete':
        currentModelMsg = null;
        break;
      case 'toolCallEvent':
        chatMsg('tool', `${msg.name}(${truncate(msg.args, 120)}) → ${truncate(msg.result, 160)}`);
        break;
      case 'flowStatus':
        liveStatus = msg.status || null;
        renderCanvas();
        renderRunFlowState();
        break;
      case 'error':
        chatMsg('error', msg.message);
        break;
      default:
        break;
    }
  }

  function renderRunFlowState() {
    if (!liveStatus) return;
    $('fs-run-flowstate').hidden = false;
    $('fs-run-active').textContent = (liveStatus.active || []).join(', ') || '—';
    $('fs-run-allowed').textContent = (liveStatus.allowed_tools || []).join(', ') || '—';
    $('fs-run-missing').textContent = (liveStatus.missing_requirements || []).join(', ') || 'none';
    const blocked = $('fs-run-blocked');
    const entries = Object.entries(liveStatus.blocked_tools || {});
    blocked.innerHTML = entries.length
      ? entries.map(([t, r]) => `<div class="fs-blocked-row">${esc(t)} <span>— ${esc(r)}</span></div>`).join('')
      : '—';
    // Per-active-step guard truth trees: exactly which atom each stuck step
    // is waiting on.
    const progress = $('fs-run-progress');
    const traces = Object.entries(liveStatus.active_progress || {});
    progress.innerHTML = traces.length
      ? traces.map(([stepId, trace]) =>
          `<div class="fs-trace-step">${esc(stepId)}</div>${traceHtml(trace)}`).join('')
      : '—';
  }

  function traceHtml(node, depth = 0) {
    const cls = node.holds ? 'holds' : 'waiting';
    let html = `<div class="fs-trace-node ${cls}" style="margin-left:${depth * 14}px">${esc(node.desc)}</div>`;
    for (const child of node.children || []) html += traceHtml(child, depth + 1);
    return html;
  }

  $('fs-run-btn').addEventListener('click', () => (ws ? stopRun() : startRun()));
  $('fs-chat-form').addEventListener('submit', (e) => {
    e.preventDefault();
    const input = $('fs-chat-text');
    const text = input.value.trim();
    if (!text || !ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(JSON.stringify({ type: 'text', text }));
    chatMsg('user', text);
    currentModelMsg = null;
    input.value = '';
  });

  // ── Toolbar ────────────────────────────────────────────────────
  $('fs-app-name').addEventListener('input', (e) => { spec.name = e.target.value; persist(); });
  $('fs-new-btn').addEventListener('click', () => {
    if (steps().length && !confirm('Start a blank flow? Unsaved work is replaced.')) return;
    spec = blankSpec();
    layout = {};
    selectedId = null;
    liveStatus = null;
    commit();
    renderStepForm();
    renderFlowForm();
    renderAppForm();
  });
  $('fs-add-step-btn').addEventListener('click', () => {
    const id = uniqueStepId('step');
    steps().push({ id, after: [], allow: [], deny: [], done: { is_true: '' }, posture: null });
    // Place near the viewport center.
    const r = canvasWrap.getBoundingClientRect();
    layout[id] = {
      x: Math.round((r.width / 2 - pan.x) / zoom - 110 + (Math.random() * 60 - 30)),
      y: Math.round((r.height / 2 - pan.y) / zoom - 60 + (Math.random() * 60 - 30)),
    };
    commit();
    selectStep(id);
  });
  $('fs-layout-btn').addEventListener('click', () => { autoLayout(); commit(); });

  const examplesMenu = document.querySelector('.fs-menu');
  $('fs-examples-btn').addEventListener('click', (e) => { examplesMenu.classList.toggle('open'); e.stopPropagation(); });
  document.addEventListener('click', () => examplesMenu.classList.remove('open'));

  // The gallery is data-driven: /static/examples/flows/index.json lists every
  // bundled industry scenario.
  async function loadGallery() {
    try {
      const res = await fetch('/static/examples/flows/index.json');
      const manifest = await res.json();
      const menu = $('fs-examples-menu');
      menu.innerHTML = '';
      for (const entry of manifest.examples || []) {
        const item = document.createElement('button');
        item.innerHTML = `<span class="fs-menu-title">${esc(entry.title)}</span>`
          + `<span class="fs-menu-industry">${esc(entry.industry)}</span>`
          + `<span class="fs-menu-summary">${esc(entry.summary)}</span>`;
        item.title = entry.summary;
        item.addEventListener('click', async () => {
          try {
            const spec = await fetch(`/static/examples/flows/${entry.file}`);
            applyJson(await spec.text(), { fromImport: true });
            validate({ silent: true });
          } catch (err) {
            showDiagnostics([{ kind: 'err', m: `Could not load example: ${err.message}` }]);
          }
        });
        menu.append(item);
      }
    } catch (_) { /* gallery unavailable — menu stays empty */ }
  }
  loadGallery();

  // ── Code tab: the program this document is equivalent to ───────
  async function refreshCode() {
    try {
      const res = await fetch('/api/flows/codegen', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(exportSpec()),
      });
      const out = await res.json();
      if (out.valid) {
        $('fs-code-main').textContent = out.main_rs;
        $('fs-code-cargo').textContent = out.cargo_toml;
      } else {
        $('fs-code-main').textContent = `// spec did not parse:\n// ${(out.errors || []).join('\n// ')}`;
        $('fs-code-cargo').textContent = '';
      }
    } catch (err) {
      $('fs-code-main').textContent = `// codegen unavailable: ${err.message}`;
    }
  }
  $('fs-code-copy').addEventListener('click', async () => {
    try { await navigator.clipboard.writeText($('fs-code-main').textContent); } catch (_) { /* no-op */ }
  });
  $('fs-code-download').addEventListener('click', () => {
    const blob = new Blob([$('fs-code-main').textContent], { type: 'text/plain' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = 'main.rs';
    a.click();
    URL.revokeObjectURL(a.href);
  });

  // ── Preview: scrub an embedded test on the canvas, offline ─────
  let preview = null; // { snapshots, index }

  async function fetchPreview(testName) {
    const res = await fetch('/api/flows/simulate', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ spec: exportSpec(), test: testName }),
    });
    const out = await res.json();
    if (!out.valid) {
      showDiagnostics((out.errors || []).map((m) => ({ kind: 'err', m })));
      return null;
    }
    return out.snapshots;
  }

  function renderPreview() {
    if (!preview) return;
    const snapshot = preview.snapshots[preview.index];
    liveStatus = snapshot;
    renderCanvas();
    renderRunFlowState();
    $('fs-preview-pos').textContent = `${preview.index}/${preview.snapshots.length - 1}`;
    const failures = (snapshot.failures || []);
    $('fs-preview-event').textContent = failures.length
      ? `${snapshot.event} — ${failures[0]}`
      : snapshot.event;
    $('fs-preview-event').classList.toggle('failed', failures.length > 0);
  }

  async function enterPreview() {
    const tests = spec.tests || [];
    if (!tests.length) {
      showDiagnostics([{
        kind: 'warn',
        m: 'No tests to preview. Add a "tests" array (JSON tab) and Preview will scrub it on the canvas.',
      }]);
      return;
    }
    const select = $('fs-preview-test');
    select.innerHTML = '';
    for (const t of tests) {
      const option = document.createElement('option');
      option.value = t.name;
      option.textContent = t.name;
      select.append(option);
    }
    const snapshots = await fetchPreview(tests[0].name);
    if (!snapshots) return;
    preview = { snapshots, index: 0 };
    $('fs-preview-bar').hidden = false;
    renderPreview();
  }

  function exitPreview() {
    preview = null;
    liveStatus = null;
    $('fs-preview-bar').hidden = true;
    renderCanvas();
  }

  $('fs-preview-btn').addEventListener('click', () => (preview ? exitPreview() : enterPreview()));
  $('fs-preview-exit').addEventListener('click', exitPreview);

  // ── Open ───────────────────────────────────────────────────────
  // Download had no inverse: a saved flow came back only by pasting it into
  // the JSON tab and pressing Apply. A file picker and a drop target close
  // the loop. Both go through applyJson, so an opened file is checked and
  // normalised exactly as pasted text is.
  async function openFile(file) {
    if (!file) return;
    if (steps().length && !confirm(`Open ${file.name}? The current flow is replaced.`)) return;
    try {
      applyJson(await file.text(), { fromImport: true });
    } catch (err) {
      const status = $('fs-json-status');
      status.textContent = `Could not read ${file.name}: ${err.message}`;
      status.className = 'fs-json-status err';
    }
  }
  $('fs-open-btn').addEventListener('click', () => $('fs-open-input').click());
  $('fs-open-input').addEventListener('change', (e) => {
    openFile(e.target.files[0]);
    e.target.value = ''; // so the same file can be re-opened after a New
  });
  const dropTarget = $('fs-canvas-wrap');
  dropTarget.addEventListener('dragover', (e) => {
    if (!Array.from(e.dataTransfer.types).includes('Files')) return;
    e.preventDefault();
    dropTarget.classList.add('fs-drop');
  });
  dropTarget.addEventListener('dragleave', () => dropTarget.classList.remove('fs-drop'));
  dropTarget.addEventListener('drop', (e) => {
    e.preventDefault();
    dropTarget.classList.remove('fs-drop');
    openFile(e.dataTransfer.files[0]);
  });

  // ── Keyboard ───────────────────────────────────────────────────
  // Modifier chords work everywhere, like any editor. The bare keys only
  // act when no field has focus, so Backspace in the step-id box edits the
  // id rather than deleting the step it belongs to.
  function typing(el) {
    return !!el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA'
      || el.tagName === 'SELECT' || el.isContentEditable);
  }
  document.addEventListener('keydown', (e) => {
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key === 'Enter') { e.preventDefault(); validate(); return; }
    if (mod && !e.shiftKey && e.key.toLowerCase() === 's') { e.preventDefault(); $('fs-json-download').click(); return; }
    if (mod && e.key.toLowerCase() === 'o') { e.preventDefault(); $('fs-open-input').click(); return; }
    if (typing(e.target)) return;
    if (e.key === 'Escape') {
      if (preview) { exitPreview(); return; }
      if (!$('fs-diagnostics').hidden) { $('fs-diagnostics').hidden = true; return; }
      if (selectedId) { selectedId = null; renderCanvas(); renderStepForm(); }
      return;
    }
    if ((e.key === 'Delete' || e.key === 'Backspace') && selectedId && !preview) {
      e.preventDefault();
      deleteStep(selectedId);
    }
  });
  $('fs-preview-prev').addEventListener('click', () => {
    if (preview && preview.index > 0) { preview.index -= 1; renderPreview(); }
  });
  $('fs-preview-next').addEventListener('click', () => {
    if (preview && preview.index < preview.snapshots.length - 1) { preview.index += 1; renderPreview(); }
  });
  $('fs-preview-test').addEventListener('change', async (e) => {
    const snapshots = await fetchPreview(e.target.value);
    if (snapshots) { preview = { snapshots, index: 0 }; renderPreview(); }
  });

  // ── Tabs ───────────────────────────────────────────────────────
  function switchTab(name) {
    document.querySelectorAll('.fs-tab').forEach((t) => t.classList.toggle('active', t.dataset.tab === name));
    document.querySelectorAll('.fs-pane').forEach((p) => p.classList.toggle('active', p.id === `fs-pane-${name}`));
    if (name === 'json') syncJsonPane();
    if (name === 'flow') renderFlowForm();
    if (name === 'app') renderAppForm();
    if (name === 'code') refreshCode();
  }
  document.querySelectorAll('.fs-tab').forEach((t) => t.addEventListener('click', () => switchTab(t.dataset.tab)));

  // ── Utilities ──────────────────────────────────────────────────
  function esc(s) {
    return String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  }
  const truncate = (s, n) => (String(s).length > n ? `${String(s).slice(0, n)}…` : String(s));

  // ── Boot ───────────────────────────────────────────────────────
  restore();
  if (steps().length && Object.values(layout).every((p) => !p.x && !p.y)) autoLayout();
  renderCanvas();
  renderStepForm();
  renderFlowForm();
  renderAppForm();
  syncJsonPane();
})();
