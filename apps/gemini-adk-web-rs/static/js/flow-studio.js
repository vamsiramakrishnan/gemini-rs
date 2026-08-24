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
    setBadge('idle', 'not validated');
    const jsonText = $('fs-json-text');
    if (document.activeElement !== jsonText) syncJsonPane();
    persist();
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
      const deps = (s?.after || []).filter((d) => ids.includes(d));
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
        if (!findStep(dep)) continue;
        const a = nodeAnchor(dep, 'out');
        const b = nodeAnchor(step.id, 'in');
        svg += `<path class="fs-edge" data-from="${esc(dep)}" data-to="${esc(step.id)}" `
          + `d="${edgePath(a, b)}" marker-end="url(#fs-arrow)"><title>${esc(dep)} → ${esc(step.id)} (click to remove)</title></path>`;
      }
    }
    svg += '<path id="fs-ghost-edge" class="fs-edge-ghost" d="" style="display:none"/>';
    edgesSvg.innerHTML = svg;
    edgesSvg.querySelectorAll('.fs-edge').forEach((p) => {
      p.addEventListener('click', (e) => {
        const to = findStep(p.dataset.to);
        if (to) to.after = to.after.filter((d) => d !== p.dataset.from);
        commit();
        if (selectedId === p.dataset.to) renderStepForm();
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
        if (t && !t.after.includes(fromId)) t.after.push(fromId);
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

    form.append(field('Runs after', textInput(csv(step.after), (v) => { step.after = parseCsv(v); softCommit(); },
      { mono: true, placeholder: 'step ids' }),
      'Dependency edges. You can also drag between nodes on the canvas.'));

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
    for (const s of steps()) s.after = (s.after || []).map((d) => (d === from ? to : d));
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
    for (const s of steps()) s.after = (s.after || []).filter((d) => d !== id);
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
        i.value = v;
        i.addEventListener('input', () => setArg(i.value));
        root.append(i);
      } else if (k === 'eq') {
        const key = document.createElement('input');
        key.placeholder = 'state key';
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
      + '<option value="require">require — steps needed for completion</option>';
    sel.style.flex = '1';
    const add = document.createElement('button');
    add.className = 'fs-btn';
    add.textContent = 'Add';
    add.addEventListener('click', () => {
      const kind = sel.value;
      const fresh = kind === 'once' ? { once: '' }
        : kind === 'never_until' ? { never_until: { tool: '', until: { is_true: '' } } }
        : kind === 'before' ? { before: ['', ''] }
        : { require: [] };
      spec.flow.constraints.push(fresh);
      softCommit();
      renderFlowForm();
    });
    addRow.append(sel, add);
    form.append(addRow);
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
