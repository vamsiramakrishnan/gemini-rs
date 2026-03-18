# Console UI Redesign — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Rebuild the apps/adk-web devtools panel into an OTel-native trace explorer with virtualized rendering, unified timeline, canvas minimap, token/usage metrics, and a SessionBridge to eliminate app boilerplate.

**Architecture:** Frontend-first (Tasks 1-6 are pure JS/CSS, no Rust changes). Then backend (Tasks 7-10 add Rust types + OTel bridging). Frontend gracefully handles missing new message types so both halves are independently deployable.

**Tech Stack:** Vanilla JS (no frameworks/bundlers), `<canvas>` for minimap + sparkline, Axum + serde for server messages, `tracing::Layer` for OTel span bridging.

---

### Task 1: Rendering Primitives — RingBuffer, VirtualList, RenderScheduler

**Files:**
- Create: `apps/adk-web/static/js/render/ring-buffer.js`
- Create: `apps/adk-web/static/js/render/virtual-list.js`
- Create: `apps/adk-web/static/js/render/render-scheduler.js`

These three modules are the performance foundation. Everything else builds on them.

**Step 1: Create `ring-buffer.js`**

```js
/**
 * ring-buffer.js — Fixed-capacity ring buffer with O(1) append.
 * Oldest entries silently dropped when full.
 */
class RingBuffer {
  constructor(capacity) {
    this._cap = capacity;
    this._buf = new Array(capacity);
    this._head = 0;   // next write index
    this._len = 0;    // current item count
  }

  push(item) {
    this._buf[this._head] = item;
    this._head = (this._head + 1) % this._cap;
    if (this._len < this._cap) this._len++;
  }

  get length() { return this._len; }

  /** Get item at logical index (0 = oldest). */
  get(i) {
    if (i < 0 || i >= this._len) return undefined;
    const realIdx = (this._head - this._len + i + this._cap) % this._cap;
    return this._buf[realIdx];
  }

  /** Get the most recent item. */
  last() { return this._len > 0 ? this.get(this._len - 1) : undefined; }

  clear() {
    this._head = 0;
    this._len = 0;
  }

  /** Iterate over all items oldest-first, calling fn(item, logicalIndex). */
  forEach(fn) {
    for (let i = 0; i < this._len; i++) {
      fn(this.get(i), i);
    }
  }

  /** Return a new array of items matching the predicate. */
  filter(pred) {
    const out = [];
    for (let i = 0; i < this._len; i++) {
      const item = this.get(i);
      if (pred(item, i)) out.push(item);
    }
    return out;
  }
}
```

**Step 2: Create `virtual-list.js`**

```js
/**
 * virtual-list.js — Renders only visible rows via DOM recycling.
 *
 * Usage:
 *   const vl = new VirtualList(container, {
 *     rowHeight: 28,
 *     poolSize: 80,
 *     render: (el, item, index) => { el.textContent = item.text; }
 *   });
 *   vl.setItems(ringBufferOrArray);
 *   vl.scrollToBottom();
 */
class VirtualList {
  constructor(container, opts) {
    this._container = container;
    this._rowHeight = opts.rowHeight || 28;
    this._render = opts.render;
    this._poolSize = opts.poolSize || 80;

    // Data source — either a RingBuffer or plain array
    this._items = null;
    this._itemCount = 0;

    // Filtered view indices (null = show all)
    this._filteredIndices = null;

    // DOM setup
    this._viewport = document.createElement('div');
    this._viewport.className = 'vl-viewport';
    this._viewport.style.cssText = 'overflow-y:auto;height:100%;position:relative;contain:strict;';

    this._spacer = document.createElement('div');
    this._spacer.className = 'vl-spacer';
    this._spacer.style.cssText = 'width:1px;pointer-events:none;';

    this._pool = [];
    for (let i = 0; i < this._poolSize; i++) {
      const el = document.createElement('div');
      el.className = 'vl-row';
      el.style.cssText = 'position:absolute;left:0;right:0;will-change:transform;contain:layout style paint;';
      el.style.height = this._rowHeight + 'px';
      el.dataset.vlIndex = '-1';
      this._viewport.appendChild(el);
      this._pool.push(el);
    }

    this._viewport.appendChild(this._spacer);
    container.appendChild(this._viewport);

    // Scroll tracking
    this._lastScrollTop = 0;
    this._autoScroll = true;
    this._viewport.addEventListener('scroll', () => this._onScroll(), { passive: true });

    this._rafPending = false;
  }

  /** Set data source. Accepts RingBuffer or Array. */
  setItems(items) {
    this._items = items;
    this._update();
  }

  /** Apply a filter (array of visible logical indices). Pass null to clear. */
  setFilter(indices) {
    this._filteredIndices = indices;
    this._update();
  }

  /** Force a re-render of visible rows (e.g. after data mutation). */
  refresh() { this._update(); }

  scrollToBottom() {
    this._autoScroll = true;
    this._viewport.scrollTop = this._viewport.scrollHeight;
  }

  _getCount() {
    if (this._filteredIndices) return this._filteredIndices.length;
    if (!this._items) return 0;
    return typeof this._items.length === 'number' ? this._items.length : 0;
  }

  _getItem(visibleIdx) {
    if (!this._items) return null;
    const realIdx = this._filteredIndices ? this._filteredIndices[visibleIdx] : visibleIdx;
    return typeof this._items.get === 'function' ? this._items.get(realIdx) : this._items[realIdx];
  }

  _onScroll() {
    const st = this._viewport.scrollTop;
    const sh = this._viewport.scrollHeight;
    const ch = this._viewport.clientHeight;
    // Auto-scroll detection: are we within 2 rows of bottom?
    this._autoScroll = (sh - st - ch) < this._rowHeight * 2;
    if (!this._rafPending) {
      this._rafPending = true;
      requestAnimationFrame(() => {
        this._rafPending = false;
        this._renderVisible();
      });
    }
  }

  _update() {
    const count = this._getCount();
    this._itemCount = count;
    this._spacer.style.height = (count * this._rowHeight) + 'px';
    this._renderVisible();
    if (this._autoScroll) {
      this._viewport.scrollTop = this._viewport.scrollHeight;
    }
  }

  _renderVisible() {
    const count = this._itemCount;
    const scrollTop = this._viewport.scrollTop;
    const viewHeight = this._viewport.clientHeight;

    const startIdx = Math.max(0, Math.floor(scrollTop / this._rowHeight) - 2);
    const endIdx = Math.min(count, Math.ceil((scrollTop + viewHeight) / this._rowHeight) + 2);

    for (let p = 0; p < this._pool.length; p++) {
      const el = this._pool[p];
      const idx = startIdx + p;
      if (idx >= startIdx && idx < endIdx && idx < count) {
        const item = this._getItem(idx);
        if (item && el.dataset.vlIndex !== String(idx)) {
          el.style.transform = 'translateY(' + (idx * this._rowHeight) + 'px)';
          el.dataset.vlIndex = String(idx);
          this._render(el, item, idx);
          el.style.display = '';
        } else if (item) {
          // Position may have shifted even if index unchanged
          el.style.transform = 'translateY(' + (idx * this._rowHeight) + 'px)';
        }
      } else {
        el.style.display = 'none';
        el.dataset.vlIndex = '-1';
      }
    }
  }

  destroy() {
    this._viewport.remove();
  }
}
```

**Step 3: Create `render-scheduler.js`**

```js
/**
 * render-scheduler.js — Single rAF loop coalescing all panel renders.
 *
 * Usage:
 *   const sched = new RenderScheduler();
 *   sched.register('timeline', () => renderTimeline());
 *   sched.register('metrics', () => renderMetrics());
 *   sched.markDirty('timeline');  // will render on next frame
 */
class RenderScheduler {
  constructor() {
    this._renderers = {};
    this._dirty = new Set();
    this._running = false;
    this._rafId = null;
  }

  register(name, renderFn) {
    this._renderers[name] = renderFn;
  }

  unregister(name) {
    delete this._renderers[name];
    this._dirty.delete(name);
  }

  markDirty(name) {
    this._dirty.add(name);
    this._ensureRunning();
  }

  _ensureRunning() {
    if (this._running) return;
    this._running = true;
    this._tick();
  }

  _tick() {
    this._rafId = requestAnimationFrame(() => {
      if (this._dirty.size > 0) {
        for (const name of this._dirty) {
          const fn = this._renderers[name];
          if (fn) fn();
        }
        this._dirty.clear();
      }
      // Keep running as long as we have registered renderers
      if (Object.keys(this._renderers).length > 0) {
        this._tick();
      } else {
        this._running = false;
      }
    });
  }

  stop() {
    if (this._rafId) cancelAnimationFrame(this._rafId);
    this._running = false;
    this._dirty.clear();
  }
}
```

**Step 4: Verify files load without errors**

Run: Open browser devtools console, load the app page, verify no JS errors from the new files.

**Step 5: Commit**

```bash
git add apps/adk-web/static/js/render/
git commit -m "feat(ui): add rendering primitives — RingBuffer, VirtualList, RenderScheduler"
```

---

### Task 2: Timeline Panel — Replace Events Tab

**Files:**
- Modify: `apps/adk-web/static/app.html` — add `<script>` tags for render modules, add minimap canvas placeholder
- Rewrite: `apps/adk-web/static/js/devtools.js` — new `DevtoolsManager` using rendering primitives
- Modify: `apps/adk-web/static/css/devtools.css` — new timeline row styles

This is the largest task. The entire `DevtoolsManager` class is rewritten to use `RingBuffer` + `VirtualList` for the timeline, with the new tab structure: Timeline | State | Phases | Metrics.

**Step 1: Update `app.html` to load render modules**

Add before the existing `<script>` tags:
```html
<script src="/static/js/render/ring-buffer.js"></script>
<script src="/static/js/render/virtual-list.js"></script>
<script src="/static/js/render/render-scheduler.js"></script>
```

Add a `<canvas>` element inside the devtools-pane, between the tab bar and status bar:
```html
<canvas class="minimap-canvas" id="minimap-canvas" height="24"></canvas>
```

**Step 2: Rewrite `devtools.js` with the new architecture**

The new `DevtoolsManager` must:

- Store all events in a `RingBuffer(10000)` instead of an unbounded array.
- Use `VirtualList` for the timeline panel instead of appending DOM nodes.
- Use a `RenderScheduler` to coalesce dirty panel renders.
- Restructure tabs to: `timeline` (default), `state`, `phases`, `metrics`.
- Timeline row rendering: single monospaced line with `[time] [TYPE] content [duration]` format.
- Filter toolbar with toggle buttons per event type, persisted to `localStorage`.
- Search input with 150ms debounce filtering timeline rows by substring.
- Click-to-expand rows showing full JSON payload.
- Merge evaluator events into timeline as colored rows (no separate Evaluator tab).

Key internal structure:
```js
class DevtoolsManager {
  constructor(container) {
    this.scheduler = new RenderScheduler();
    this.events = new RingBuffer(10000);
    this.stateData = {};       // key -> { value, prevValue, el }
    this.phases = [];
    this.telemetry = {};
    this.turnLatencies = [];   // per-turn latency for sparkline
    this.toolCalls = [];
    // ... filter state, search state, DOM refs ...

    this.scheduler.register('timeline', () => this._renderTimeline());
    this.scheduler.register('state', () => this._renderState());
    this.scheduler.register('phases', () => this._renderPhases());
    this.scheduler.register('metrics', () => this._renderMetrics());
    this.scheduler.register('minimap', () => this._renderMinimap());
    this.scheduler.register('statusBar', () => this._renderStatusBar());
  }
}
```

The timeline VirtualList row renderer:
```js
_renderTimelineRow(el, event, idx) {
  // Single-line: [time] [badge] [content] [duration]
  el.className = 'tl-row ' + event.type;
  el.innerHTML = '';

  const time = el._timeEl || (el._timeEl = document.createElement('span'));
  time.className = 'tl-time';
  time.textContent = event.time;

  const badge = el._badgeEl || (el._badgeEl = document.createElement('span'));
  badge.className = 'tl-badge ' + event.type;
  badge.textContent = this._badgeLabel(event.type);

  const content = el._contentEl || (el._contentEl = document.createElement('span'));
  content.className = 'tl-content';
  content.textContent = event.summary;

  // Reuse existing child nodes or append
  if (!el.firstChild) {
    el.appendChild(time);
    el.appendChild(badge);
    el.appendChild(content);
    if (event.duration) {
      const dur = document.createElement('span');
      dur.className = 'tl-duration';
      dur.textContent = event.duration;
      el.appendChild(dur);
      el._durEl = dur;
    }
  } else {
    // Update in place — no DOM creation
    if (el._durEl) {
      el._durEl.textContent = event.duration || '';
      el._durEl.style.display = event.duration ? '' : 'none';
    }
  }
}
```

The addEvent method:
```js
addEvent(msg) {
  const elapsed = Date.now() - this.sessionStart;
  const event = {
    type: msg.type,
    time: this._fmtTime(elapsed),
    timeMs: elapsed,
    summary: this._summarize(msg),
    duration: this._extractDuration(msg),
    raw: msg,
  };
  this.events.push(event);
  this.scheduler.markDirty('timeline');
  this.scheduler.markDirty('minimap');
}
```

**Step 3: Add timeline CSS to `devtools.css`**

Replace old `.event-entry` styles with new `.tl-row` styles:
```css
.tl-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 12px;
  font-family: var(--mono);
  font-size: 12px;
  line-height: 28px;      /* matches VirtualList rowHeight */
  cursor: pointer;
  border-bottom: 1px solid var(--border-light);
  contain: layout style paint;
}
.tl-row:hover { background: rgba(66,133,244,0.04); }
.tl-row.expanded { height: auto; min-height: 28px; }

.tl-time { color: var(--text-muted); font-size: 11px; min-width: 56px; flex-shrink: 0; }
.tl-badge {
  display: inline-block;
  padding: 1px 5px;
  border-radius: 3px;
  font-size: 9px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.03em;
  flex-shrink: 0;
  min-width: 44px;
  text-align: center;
}
.tl-content { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tl-duration { color: var(--text-secondary); font-size: 11px; flex-shrink: 0; }

/* Badge colors — reuse existing palette */
.tl-badge.textDelta, .tl-badge.textComplete { background: #e8f0fe; color: #1967d2; }
.tl-badge.audio { background: #f3e8fd; color: #7627bb; }
.tl-badge.turnComplete { background: #e6f4ea; color: #137333; }
.tl-badge.stateUpdate { background: #fef7e0; color: #b06000; }
.tl-badge.phaseChange { background: #fff3e0; color: #e65100; }
.tl-badge.toolCallEvent { background: #e8f0fe; color: #1967d2; }
.tl-badge.violation { background: #fce8e6; color: #c5221f; }
.tl-badge.spanEvent { background: #e0f7fa; color: #00695c; }
.tl-badge.connected, .tl-badge.appMeta { background: #f1f3f4; color: #5f6368; }
.tl-badge.interrupted, .tl-badge.error { background: #fce8e6; color: #c5221f; }
/* Catch-all for unrecognized types */
.tl-badge { background: #f1f3f4; color: #5f6368; }
```

Filter toolbar:
```css
.tl-filters {
  display: flex; align-items: center; gap: 4px;
  padding: 4px 10px; border-bottom: 1px solid var(--border-light);
  background: var(--devtools-bg); flex-shrink: 0; font-size: 11px;
}
.tl-filter-btn {
  padding: 2px 7px; border-radius: 3px; border: 1px solid var(--border-light);
  background: #fff; cursor: pointer; font-size: 10px; font-weight: 600;
  text-transform: uppercase; letter-spacing: 0.03em; color: var(--text-secondary);
  transition: opacity 0.15s;
}
.tl-filter-btn.hidden { opacity: 0.4; text-decoration: line-through; }
.tl-search {
  margin-left: auto; padding: 3px 8px; border: 1px solid var(--border-light);
  border-radius: 4px; font-size: 11px; font-family: var(--mono);
  width: 120px; outline: none; background: #fff;
}
.tl-search:focus { border-color: var(--primary); }
```

**Step 4: Update `app.js` to forward events to the new devtools API**

The `handleMessage` function in `app.js` already calls `devtools.addEvent(msg)` and `devtools.handleStateUpdate()`, etc. The API surface stays the same — the new `DevtoolsManager` just handles messages differently internally. Verify the following methods still exist on the new class:
- `addEvent(msg)`
- `handleStateUpdate(key, value)`
- `handlePhaseChange(data)`
- `handleEvaluation(data)`
- `handleViolation(data)`
- `handleTelemetry(stats)`
- `handlePhaseTimeline(entries)`
- `handleToolCallEvent(data)`
- `handleAppMeta(info)`
- `reset()`
- `expand()` / `toggleCollapse()`

**Step 5: Test manually**

Run: `cargo run -p rs-genai-ui` and connect to a text-chat or tool-calling app. Verify:
- Timeline shows events in chronological order with colored badges
- Filter buttons toggle event visibility
- Search filters rows by content
- Scroll stays at bottom during live events (auto-scroll)
- No visible jank during rapid event bursts
- Tabs switch correctly: Timeline, State, Phases, Metrics

**Step 6: Commit**

```bash
git add apps/adk-web/static/js/devtools.js apps/adk-web/static/app.html apps/adk-web/static/css/devtools.css
git commit -m "feat(ui): unified timeline panel with VirtualList rendering"
```

---

### Task 3: Canvas Minimap

**Files:**
- Create: `apps/adk-web/static/js/render/minimap.js`
- Modify: `apps/adk-web/static/js/devtools.js` — integrate minimap
- Modify: `apps/adk-web/static/css/devtools.css` — minimap container styles

**Step 1: Create `minimap.js`**

```js
/**
 * minimap.js — Canvas-rendered event density minimap.
 *
 * Draws colored 1-2px ticks for each event, with a viewport overlay
 * showing which portion of the timeline is currently visible.
 */
class Minimap {
  constructor(canvas, opts) {
    this._canvas = canvas;
    this._ctx = canvas.getContext('2d');
    this._events = null;       // RingBuffer reference
    this._viewStart = 0;       // visible range start (ratio 0-1)
    this._viewEnd = 1;         // visible range end (ratio 0-1)
    this._onClick = opts.onClick || null;
    this._sessionDurationMs = 0;

    // Color map for event types
    this._colors = {
      audio: '#7627bb',
      textDelta: '#1967d2', textComplete: '#1967d2',
      stateUpdate: '#b06000',
      phaseChange: '#e65100',
      toolCallEvent: '#1967d2',
      turnComplete: '#137333',
      interrupted: '#c5221f', error: '#c5221f',
      violation: '#c5221f',
      spanEvent: '#00695c',
      voiceActivityStart: '#7627bb', voiceActivityEnd: '#7627bb',
      connected: '#5f6368', appMeta: '#5f6368',
    };
    this._defaultColor = '#9aa0a6';

    canvas.addEventListener('click', (e) => {
      if (!this._onClick || !this._events || this._events.length === 0) return;
      const rect = canvas.getBoundingClientRect();
      const ratio = (e.clientX - rect.left) / rect.width;
      this._onClick(ratio);
    });
  }

  setEvents(events) { this._events = events; }
  setSessionDuration(ms) { this._sessionDurationMs = ms; }
  setViewport(startRatio, endRatio) {
    this._viewStart = startRatio;
    this._viewEnd = endRatio;
  }

  render() {
    const c = this._canvas;
    const ctx = this._ctx;
    const w = c.width = c.clientWidth * (window.devicePixelRatio || 1);
    const h = c.height = 24 * (window.devicePixelRatio || 1);
    ctx.clearRect(0, 0, w, h);

    if (!this._events || this._events.length === 0 || this._sessionDurationMs <= 0) return;

    const dur = this._sessionDurationMs;
    const dpr = window.devicePixelRatio || 1;

    // Draw event ticks
    this._events.forEach((ev) => {
      const x = (ev.timeMs / dur) * w;
      const color = this._colors[ev.type] || this._defaultColor;
      ctx.fillStyle = color;
      ctx.globalAlpha = 0.7;
      ctx.fillRect(Math.floor(x), 0, Math.max(1, 1 * dpr), h);
    });

    // Draw viewport overlay
    ctx.globalAlpha = 0.15;
    ctx.fillStyle = '#4285f4';
    const vx = this._viewStart * w;
    const vw = (this._viewEnd - this._viewStart) * w;
    ctx.fillRect(vx, 0, Math.max(vw, 2), h);

    // Viewport border
    ctx.globalAlpha = 0.5;
    ctx.strokeStyle = '#4285f4';
    ctx.lineWidth = 1 * dpr;
    ctx.strokeRect(vx, 0, Math.max(vw, 2), h);

    ctx.globalAlpha = 1;
  }
}
```

**Step 2: Add minimap CSS**

```css
.minimap-canvas {
  width: 100%;
  height: 24px;
  display: block;
  border-bottom: 1px solid var(--border-light);
  cursor: pointer;
  contain: size layout paint;
  flex-shrink: 0;
}
```

**Step 3: Integrate into `DevtoolsManager`**

In the constructor, after creating the timeline VirtualList:
```js
this._minimap = new Minimap(document.getElementById('minimap-canvas'), {
  onClick: (ratio) => {
    // Scroll timeline to the corresponding position
    const idx = Math.floor(ratio * this._getFilteredCount());
    this._timelineVL.scrollToIndex(idx);
  }
});
this._minimap.setEvents(this.events);
this.scheduler.register('minimap', () => {
  this._minimap.setSessionDuration(Date.now() - this.sessionStart);
  this._minimap.render();
});
```

Update `addEvent` to also mark minimap dirty (already handled by scheduler).

Connect the timeline VirtualList scroll position to the minimap viewport overlay:
```js
// In timeline scroll handler:
const viewStart = scrollTop / totalHeight;
const viewEnd = (scrollTop + viewHeight) / totalHeight;
this._minimap.setViewport(viewStart, viewEnd);
this.scheduler.markDirty('minimap');
```

**Step 4: Test manually**

- Verify colored ticks appear as events flow in
- Verify viewport overlay tracks scroll position
- Verify click on minimap scrolls timeline
- Verify repaint is smooth (< 0.5ms — check via `performance.now()` in devtools)

**Step 5: Commit**

```bash
git add apps/adk-web/static/js/render/minimap.js apps/adk-web/static/js/devtools.js apps/adk-web/static/css/devtools.css
git commit -m "feat(ui): canvas minimap with click-to-scroll and viewport overlay"
```

---

### Task 4: Metrics Panel — Tokens, Cost, Sparkline

**Files:**
- Create: `apps/adk-web/static/js/render/sparkline.js`
- Modify: `apps/adk-web/static/js/devtools.js` — new `_renderMetrics()` method
- Modify: `apps/adk-web/static/css/devtools.css` — metrics panel styles

**Step 1: Create `sparkline.js`**

```js
/**
 * sparkline.js — Tiny canvas bar chart for per-turn latency visualization.
 */
class Sparkline {
  constructor(canvas) {
    this._canvas = canvas;
    this._ctx = canvas.getContext('2d');
    this._data = []; // array of numbers
  }

  setData(arr) { this._data = arr; }

  render() {
    const c = this._canvas;
    const ctx = this._ctx;
    const dpr = window.devicePixelRatio || 1;
    const w = c.width = c.clientWidth * dpr;
    const h = c.height = c.clientHeight * dpr;
    ctx.clearRect(0, 0, w, h);

    const data = this._data;
    if (data.length === 0) return;

    const max = Math.max(...data, 1);
    const barW = Math.max(2, Math.floor(w / Math.max(data.length, 1)) - 1);
    const gap = 1;

    for (let i = 0; i < data.length; i++) {
      const val = data[i];
      const barH = Math.max(1, (val / max) * h * 0.9);
      const x = i * (barW + gap);
      const y = h - barH;

      // Color by health: green < 300ms, amber < 600ms, red >= 600ms
      ctx.fillStyle = val < 300 ? '#1b873f' : val < 600 ? '#d4820c' : '#c5221f';
      ctx.fillRect(x, y, barW, barH);
    }
  }
}
```

**Step 2: Update `_renderMetrics()` in `devtools.js`**

The new metrics panel renders three hero columns (Latency, Tokens, Session) plus the existing range visualization, sparkline, audio section, and tool calls list. The key change is surfacing token fields that `SessionTelemetry::snapshot()` already sends but the current UI ignores:

```js
_renderMetrics() {
  const stats = this.telemetry;
  if (!stats || Object.keys(stats).length === 0) {
    this.panels.metrics.innerHTML = '<div class="panel-empty">No metrics yet</div>';
    return;
  }

  const totalTokens = stats.total_token_count || 0;
  const promptTokens = stats.prompt_token_count || 0;
  const responseTokens = stats.response_token_count || 0;
  const cachedTokens = stats.cached_content_token_count || 0;

  // Cost estimate (Gemini 2.0 Flash pricing: ~$0.075/1M input, ~$0.30/1M output)
  const costEst = (promptTokens * 0.000000075 + responseTokens * 0.0000003).toFixed(6);

  // Build HTML with three hero columns...
  let html = '<div class="metrics-content">';
  html += '<div class="metrics-heroes">';

  // ... Latency hero (kept from current NFR) ...
  // ... Token hero (NEW) ...
  html += `<div class="metrics-hero">
    <div class="metrics-hero-label">Tokens</div>
    <div class="metrics-hero-value">${totalTokens.toLocaleString()}</div>
    <div class="metrics-hero-sub">
      ${promptTokens.toLocaleString()} prompt / ${responseTokens.toLocaleString()} response
    </div>
    ${cachedTokens > 0 ? `<div class="metrics-hero-sub">${cachedTokens.toLocaleString()} cached</div>` : ''}
    <div class="metrics-hero-sub">~$${costEst}</div>
  </div>`;

  // ... Session hero (uptime, turns, interruptions, current phase) ...
  html += '</div>'; // end heroes

  // Sparkline canvas for per-turn latency
  html += '<div class="metrics-sparkline-wrap"><canvas class="metrics-sparkline" id="sparkline-canvas"></canvas></div>';

  // Audio + tool calls sections (kept from current NFR, cleaned up)
  // ...
  html += '</div>';
  this.panels.metrics.innerHTML = html;

  // Render sparkline after DOM update
  const sparkCanvas = document.getElementById('sparkline-canvas');
  if (sparkCanvas && this.turnLatencies.length > 0) {
    if (!this._sparkline) this._sparkline = new Sparkline(sparkCanvas);
    else this._sparkline._canvas = sparkCanvas;
    this._sparkline.setData(this.turnLatencies);
    this._sparkline.render();
  }
}
```

Track per-turn latencies:
```js
handleTelemetry(stats) {
  const prevCount = this.telemetry.response_count || 0;
  this.telemetry = stats;

  // If response_count increased, record the latest latency for sparkline
  if (stats.response_count > prevCount && stats.last_response_latency_ms > 0) {
    this.turnLatencies.push(stats.last_response_latency_ms);
  }

  this.scheduler.markDirty('metrics');
  this.scheduler.markDirty('statusBar');
}
```

**Step 3: Add metrics CSS**

```css
.metrics-content { padding: 12px; display: flex; flex-direction: column; gap: 8px; }
.metrics-heroes { display: flex; gap: 8px; }
.metrics-hero {
  flex: 1; padding: 12px; border-radius: 8px;
  border: 1px solid var(--border-light); background: #fff;
}
.metrics-hero-label {
  font-size: 10px; font-weight: 700; text-transform: uppercase;
  letter-spacing: 0.06em; color: var(--text-muted); margin-bottom: 4px;
}
.metrics-hero-value {
  font-size: 24px; font-weight: 700; font-family: var(--mono);
  color: var(--text-primary); line-height: 1.1; margin-bottom: 4px;
}
.metrics-hero-sub {
  font-size: 11px; font-family: var(--mono); color: var(--text-secondary); line-height: 1.4;
}
.metrics-sparkline-wrap { height: 32px; }
.metrics-sparkline { width: 100%; height: 32px; display: block; }
```

**Step 4: Test**

- Connect to a voice-chat app (generates telemetry with token counts)
- Verify token counts, cost estimate, and sparkline render
- Verify sparkline adds bars as turns complete
- Verify metrics update smoothly via RenderScheduler (no flickering)

**Step 5: Commit**

```bash
git add apps/adk-web/static/js/render/sparkline.js apps/adk-web/static/js/devtools.js apps/adk-web/static/css/devtools.css
git commit -m "feat(ui): metrics panel with token counts, cost estimation, and latency sparkline"
```

---

### Task 5: State Panel — Targeted Updates, Diff Flash, Search

**Files:**
- Modify: `apps/adk-web/static/js/devtools.js` — new state panel with `Map<key, DOMElement>` tracking
- Modify: `apps/adk-web/static/css/devtools.css` — diff flash + search styles

**Step 1: Rewrite state panel rendering in `devtools.js`**

Replace `_renderState()` with a targeted-update approach:

```js
// In constructor:
this._stateMap = new Map(); // key -> { keyEl, valueEl, row, group }
this._stateGroups = {};     // prefix -> { header, container, collapsed }
this._stateSearchTerm = '';

handleStateUpdate(key, value) {
  const prev = this.stateData[key];
  this.stateData[key] = value;

  if (this._stateMap.has(key)) {
    // Update existing cell — no DOM creation
    const entry = this._stateMap.get(key);
    const { display, className } = this._formatValue(value);
    entry.valueEl.innerHTML = display;
    entry.valueEl.className = 'state-value ' + className;

    // Flash animation
    entry.row.classList.remove('state-row-flash');
    void entry.row.offsetWidth; // force reflow for re-animation
    entry.row.classList.add('state-row-flash');

    // Show previous value
    if (prev !== undefined) {
      const prevStr = typeof prev === 'string' ? `"${prev}"` : JSON.stringify(prev);
      entry.valueEl.insertAdjacentHTML('beforeend',
        ` <span class="state-was">was: ${this._esc(this._truncStr(prevStr, 30))}</span>`);
      setTimeout(() => {
        const was = entry.valueEl.querySelector('.state-was');
        if (was) was.remove();
      }, 2000);
    }
  } else {
    // New key — create row and add to appropriate group
    this._addStateRow(key, value);
  }

  // Apply search filter
  if (this._stateSearchTerm) this._filterState(this._stateSearchTerm);
}
```

**Step 2: Add state search and collapsed groups**

```js
_initStatePanel() {
  const panel = this.panels.state;
  panel.innerHTML = '';
  panel.className = 'devtools-panel state-panel';

  // Search bar
  const search = document.createElement('input');
  search.className = 'state-search';
  search.placeholder = 'Filter keys...';
  search.addEventListener('input', () => {
    this._stateSearchTerm = search.value.toLowerCase();
    this._filterState(this._stateSearchTerm);
  });
  panel.appendChild(search);

  // Container for groups
  const groupContainer = document.createElement('div');
  groupContainer.className = 'state-groups';
  panel.appendChild(groupContainer);
  this._stateGroupContainer = groupContainer;
}

_addStateRow(key, value) {
  const colonIdx = key.indexOf(':');
  const prefix = (colonIdx > 0 && colonIdx < key.length - 1)
    ? key.substring(0, colonIdx) : null;
  const displayKey = prefix ? key.substring(colonIdx + 1) : key;

  // Get or create group
  const groupKey = prefix || '_general';
  if (!this._stateGroups[groupKey]) {
    this._createStateGroup(groupKey, prefix);
  }
  const group = this._stateGroups[groupKey];

  const row = document.createElement('tr');
  row.className = 'state-row';

  const keyCell = document.createElement('td');
  keyCell.className = 'state-key';
  keyCell.textContent = displayKey;

  const valCell = document.createElement('td');
  valCell.className = 'state-value';
  const { display, className } = this._formatValue(value);
  valCell.innerHTML = display;
  valCell.className = 'state-value ' + className;

  row.appendChild(keyCell);
  row.appendChild(valCell);
  group.tbody.appendChild(row);

  this._stateMap.set(key, { keyEl: keyCell, valueEl: valCell, row, group: groupKey });
}
```

**Step 3: Add CSS for diff flash and search**

```css
.state-search {
  width: calc(100% - 16px); margin: 8px; padding: 5px 8px;
  border: 1px solid var(--border-light); border-radius: 4px;
  font-size: 11px; font-family: var(--mono); outline: none; background: #fff;
}
.state-search:focus { border-color: var(--primary); }
.state-was {
  font-size: 10px; color: var(--text-muted); font-style: italic;
  animation: state-was-fade 2s ease-out forwards;
}
@keyframes state-was-fade {
  0% { opacity: 1; }
  70% { opacity: 1; }
  100% { opacity: 0; }
}
.state-group-header {
  padding: 6px 12px; cursor: pointer; user-select: none;
  font-size: 10px; font-weight: 700; text-transform: uppercase;
  letter-spacing: 0.06em; color: var(--text-muted); background: #f8f9fa;
  border-bottom: 1px solid var(--border-light);
}
.state-group-header::before {
  content: '\25BC'; margin-right: 6px; font-size: 8px;
  display: inline-block; transition: transform 0.15s;
}
.state-group.collapsed .state-group-header::before { transform: rotate(-90deg); }
.state-group.collapsed table { display: none; }
```

**Step 4: Test**

- Connect to a support or playbook app (generates many state updates)
- Verify state keys appear grouped by prefix
- Verify clicking a group header collapses/expands it
- Verify yellow flash on state change with "was: X" text
- Verify search input filters visible rows
- Verify no jank during rapid state update bursts

**Step 5: Commit**

```bash
git add apps/adk-web/static/js/devtools.js apps/adk-web/static/css/devtools.css
git commit -m "feat(ui): state panel with targeted DOM updates, diff flash, and search"
```

---

### Task 6: Phases Panel — Current Phase Hero, Duration Bars

**Files:**
- Modify: `apps/adk-web/static/js/devtools.js` — new `_renderPhases()` method
- Modify: `apps/adk-web/static/css/devtools.css` — phase hero and duration bar styles

**Step 1: Implement `_renderPhases()`**

```js
_renderPhases() {
  const panel = this.panels.phases;

  if (this.phases.length === 0 && !this.telemetry.current_phase) {
    panel.innerHTML = '<div class="panel-empty">No phase data yet</div>';
    return;
  }

  let html = '';

  // Current phase hero
  const currentPhase = this.telemetry.current_phase || (this.phases.length > 0 ? this.phases[this.phases.length - 1].to : null);
  if (currentPhase) {
    html += `<div class="phase-hero">
      <div class="phase-hero-label">Current Phase</div>
      <div class="phase-hero-name">${this._esc(currentPhase)}</div>
    </div>`;
  }

  // Phase transition entries with duration bars
  const totalMs = Date.now() - this.sessionStart;
  const entries = this.phaseTimeline.length > 0 ? this.phaseTimeline : this.phases;

  html += '<div class="phase-entries">';
  entries.forEach((entry, i) => {
    const isTimeline = entry.duration_secs !== undefined;
    const durationMs = isTimeline ? entry.duration_secs * 1000 : 0;
    const pct = totalMs > 0 ? Math.min(100, (durationMs / totalMs) * 100) : 0;
    const durationStr = isTimeline
      ? (entry.duration_secs < 1 ? `${(entry.duration_secs * 1000).toFixed(0)}ms` : `${entry.duration_secs.toFixed(1)}s`)
      : '';

    const isCurrent = i === entries.length - 1;

    html += `<div class="phase-entry ${isCurrent ? 'current' : ''}">
      <div class="phase-entry-header">
        <span class="phase-dot ${isCurrent ? 'active' : ''}"></span>
        <span class="phase-from">${this._esc(entry.from)}</span>
        <span class="phase-arrow">&rarr;</span>
        <span class="phase-to">${this._esc(entry.to)}</span>
        ${durationStr ? `<span class="phase-dur">${durationStr}</span>` : ''}
      </div>
      ${pct > 0 ? `<div class="phase-bar-track"><div class="phase-bar-fill" style="width:${pct}%"></div></div>` : ''}
      ${entry.reason ? `<div class="phase-reason">${this._esc(entry.reason)}</div>` : ''}
    </div>`;
  });
  html += '</div>';

  panel.innerHTML = html;
}
```

**Step 2: Add phase CSS**

```css
.phase-hero {
  padding: 14px 16px; background: var(--primary-light);
  border-bottom: 2px solid var(--primary); margin-bottom: 8px;
}
.phase-hero-label {
  font-size: 10px; font-weight: 700; text-transform: uppercase;
  letter-spacing: 0.06em; color: var(--primary-dark); margin-bottom: 2px;
}
.phase-hero-name { font-size: 18px; font-weight: 700; color: var(--primary-dark); }

.phase-entries { padding: 8px 12px; }
.phase-entry { margin-bottom: 10px; }
.phase-entry-header {
  display: flex; align-items: center; gap: 6px; font-size: 13px; margin-bottom: 4px;
}
.phase-dot {
  width: 8px; height: 8px; border-radius: 50%;
  background: var(--border); flex-shrink: 0;
}
.phase-dot.active { background: var(--primary); box-shadow: 0 0 6px rgba(66,133,244,0.4); }
.phase-from { font-weight: 600; }
.phase-arrow { color: var(--text-muted); font-size: 11px; }
.phase-to { font-weight: 600; color: var(--primary-dark); }
.phase-dur { margin-left: auto; font-family: var(--mono); font-size: 11px; color: var(--text-secondary); }

.phase-bar-track {
  height: 4px; background: var(--border-light); border-radius: 2px;
  margin: 2px 0 4px 14px; overflow: hidden;
}
.phase-bar-fill {
  height: 100%; background: var(--primary); border-radius: 2px;
  transition: width 0.3s ease;
}
.phase-reason {
  font-size: 11px; font-family: var(--mono); color: var(--text-secondary);
  margin-left: 14px;
}
```

**Step 3: Test**

- Connect to a playbook or support app (has phase transitions)
- Verify current phase hero shows at top
- Verify duration bars are proportional
- Verify transitions render with dot timeline

**Step 4: Commit**

```bash
git add apps/adk-web/static/js/devtools.js apps/adk-web/static/css/devtools.css
git commit -m "feat(ui): phases panel with current phase hero and duration bars"
```

---

### Task 7: Server — SessionBridge

**Files:**
- Create: `apps/adk-web/src/bridge.rs`
- Modify: `apps/adk-web/src/main.rs` — add `mod bridge;`
- Modify: `apps/adk-web/src/apps/voice_chat.rs` — migrate to SessionBridge
- Modify: `apps/adk-web/src/apps/tool_calling.rs` — migrate to SessionBridge
- Modify: `apps/adk-web/src/apps/text_chat.rs` — migrate to SessionBridge

**Step 1: Create `bridge.rs`**

```rust
//! SessionBridge — eliminates callback boilerplate in demo apps.
//!
//! Wires all standard event callbacks (audio, text, turn, interrupt, VAD, error,
//! transcription, telemetry) onto a Live builder in one call.

use base64::Engine;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use adk_rs_fluent::prelude::*;

use crate::app::{AppInfo, CookbookApp, ServerMessage, WsSender};

/// Bridge between a demo app's WebSocket sender and a Live session builder.
///
/// Call `bridge.wire_live(builder)` to attach all standard callbacks,
/// then `bridge.recv_loop(handle, rx)` to run the browser->Gemini forwarding loop.
pub struct SessionBridge {
    tx: WsSender,
}

impl SessionBridge {
    pub fn new(tx: WsSender) -> Self {
        Self { tx }
    }

    /// Send the Connected message to the browser.
    pub fn send_connected(&self) {
        let _ = self.tx.send(ServerMessage::Connected);
    }

    /// Send appMeta message so devtools can configure tabs.
    pub fn send_meta(&self, app: &dyn CookbookApp) {
        let _ = self.tx.send(ServerMessage::AppMeta {
            info: AppInfo {
                name: app.name().to_string(),
                description: app.description().to_string(),
                category: app.category(),
                features: app.features(),
                tips: app.tips(),
                try_saying: app.try_saying(),
            },
        });
    }

    /// Wire all standard event callbacks onto a Live builder.
    ///
    /// Attaches: on_audio, on_text, on_text_complete, on_turn_complete,
    /// on_interrupted, on_vad_start, on_vad_end, on_error, on_disconnected,
    /// on_input_transcript, on_output_transcript.
    ///
    /// The builder is returned with all callbacks attached — the app can
    /// add additional callbacks (e.g., on_tool_call) before calling `.connect()`.
    pub fn wire_live(&self, builder: LiveBuilder) -> LiveBuilder {
        let b64 = base64::engine::general_purpose::STANDARD;

        let tx_audio = self.tx.clone();
        let tx_text = self.tx.clone();
        let tx_text_complete = self.tx.clone();
        let tx_turn = self.tx.clone();
        let tx_interrupted = self.tx.clone();
        let tx_vad_start = self.tx.clone();
        let tx_vad_end = self.tx.clone();
        let tx_error = self.tx.clone();
        let tx_disconnected = self.tx.clone();
        let tx_input = self.tx.clone();
        let tx_output = self.tx.clone();

        builder
            .on_audio(move |data| {
                let encoded = b64.encode(data);
                let _ = tx_audio.send(ServerMessage::Audio { data: encoded });
            })
            .on_text(move |t| {
                let _ = tx_text.send(ServerMessage::TextDelta { text: t.to_string() });
            })
            .on_text_complete(move |t| {
                let _ = tx_text_complete.send(ServerMessage::TextComplete { text: t.to_string() });
            })
            .on_turn_complete(move || {
                let tx = tx_turn.clone();
                async move { let _ = tx.send(ServerMessage::TurnComplete); }
            })
            .on_interrupted(move || {
                let tx = tx_interrupted.clone();
                async move { let _ = tx.send(ServerMessage::Interrupted); }
            })
            .on_vad_start(move || {
                let _ = tx_vad_start.send(ServerMessage::VoiceActivityStart);
            })
            .on_vad_end(move || {
                let _ = tx_vad_end.send(ServerMessage::VoiceActivityEnd);
            })
            .on_error(move |msg| {
                let tx = tx_error.clone();
                async move { let _ = tx.send(ServerMessage::Error { message: msg }); }
            })
            .on_disconnected(move |_reason| {
                let _tx = tx_disconnected.clone();
                async move {}
            })
            .on_input_transcript(move |text, _is_final| {
                let _ = tx_input.send(ServerMessage::InputTranscription { text: text.to_string() });
            })
            .on_output_transcript(move |text, _is_final| {
                let _ = tx_output.send(ServerMessage::OutputTranscription { text: text.to_string() });
            })
    }

    /// Run the browser->Gemini forwarding loop.
    ///
    /// Handles Audio, Text, and Stop messages from the browser.
    /// Returns when the client sends Stop or disconnects.
    pub async fn recv_loop(
        &self,
        handle: &LiveHandle,
        rx: &mut mpsc::UnboundedReceiver<crate::app::ClientMessage>,
    ) {
        use crate::app::ClientMessage;

        let b64 = base64::engine::general_purpose::STANDARD;
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::Audio { data } => {
                    if let Ok(pcm_bytes) = b64.decode(&data) {
                        if let Err(e) = handle.send_audio(pcm_bytes).await {
                            tracing::warn!("Failed to send audio: {e}");
                            let _ = self.tx.send(ServerMessage::Error { message: e.to_string() });
                        }
                    }
                }
                ClientMessage::Text { text } => {
                    if let Err(e) = handle.send_text(&text).await {
                        tracing::warn!("Failed to send text: {e}");
                        let _ = self.tx.send(ServerMessage::Error { message: e.to_string() });
                    }
                }
                ClientMessage::Stop => {
                    let _ = handle.disconnect().await;
                    break;
                }
                _ => {}
            }
        }
    }

    /// Get a clone of the sender for custom callbacks.
    pub fn sender(&self) -> WsSender {
        self.tx.clone()
    }
}
```

**Step 2: Add `mod bridge;` to `main.rs`**

Add after line 13 (`mod ws_handler;`):
```rust
mod bridge;
```

**Step 3: Migrate `voice_chat.rs` to use SessionBridge**

Replace the entire `handle_session` body (~140 lines) with ~20 lines:

```rust
async fn handle_session(
    &self,
    tx: WsSender,
    mut rx: mpsc::UnboundedReceiver<ClientMessage>,
) -> Result<(), AppError> {
    let start = wait_for_start(&mut rx).await?;
    let bridge = crate::bridge::SessionBridge::new(tx);

    let selected_voice = resolve_voice(start.voice.as_deref());
    let config = build_session_config(start.model.as_deref())
        .map_err(|e| AppError::Connection(e.to_string()))?
        .response_modalities(vec![Modality::Audio])
        .voice(selected_voice)
        .enable_input_transcription()
        .enable_output_transcription()
        .system_instruction(
            start.system_instruction.as_deref()
                .unwrap_or("You are a helpful voice assistant. Keep your responses concise and conversational."),
        );

    let handle = bridge.wire_live(Live::builder())
        .connect(config)
        .await
        .map_err(|e| AppError::Connection(e.to_string()))?;

    bridge.send_connected();
    bridge.send_meta(self);
    info!("VoiceChat session connected");

    bridge.recv_loop(&handle, &mut rx).await;
    Ok(())
}
```

**Step 4: Migrate `text_chat.rs` similarly**

Same pattern — `bridge.wire_live()` replaces the 10+ `tx.clone()` lines. Text-chat uses `.text_only()` config and doesn't set voice/transcription.

**Step 5: Migrate `tool_calling.rs`**

Tool-calling adds an `on_tool_call` callback after `bridge.wire_live()`:
```rust
let tx_tool = bridge.sender();
let handle = bridge.wire_live(Live::builder())
    .on_tool_call(move |calls, _state| {
        let tx = tx_tool.clone();
        async move {
            let responses = calls.iter().map(|call| {
                let result = execute_tool(&call.name, &call.args);
                let _ = tx.send(ServerMessage::ToolCallEvent { /* ... */ });
                FunctionResponse { name: call.name.clone(), response: result, id: call.id.clone(), scheduling: None }
            }).collect();
            Some(responses)
        }
    })
    .connect(config)
    .await?;
```

**Step 6: Verify compilation**

Run: `cargo check -p rs-genai-ui`
Expected: clean compile.

**Step 7: Test**

Run: `cargo run -p rs-genai-ui`, test voice-chat, text-chat, and tool-calling apps.
Verify: identical behavior to before migration.

**Step 8: Commit**

```bash
git add apps/adk-web/src/bridge.rs apps/adk-web/src/main.rs apps/adk-web/src/apps/voice_chat.rs apps/adk-web/src/apps/text_chat.rs apps/adk-web/src/apps/tool_calling.rs
git commit -m "feat(ui): SessionBridge eliminates callback boilerplate in demo apps"
```

---

### Task 8: Server — New ServerMessage Variants

**Files:**
- Modify: `apps/adk-web/src/app.rs` — add `SpanEvent` and `TurnMetrics` variants to `ServerMessage`

**Step 1: Add new variants to `ServerMessage` enum**

In `apps/adk-web/src/app.rs`, add after the `ToolCallEvent` variant:

```rust
/// OTel span lifecycle event bridged from the tracing Layer.
SpanEvent {
    name: String,
    span_id: String,
    parent_id: Option<String>,
    duration_us: u64,
    attributes: serde_json::Value,
    status: String,
},
/// Per-turn metrics for sparkline visualization.
TurnMetrics {
    turn: u32,
    latency_ms: u32,
    prompt_tokens: u32,
    response_tokens: u32,
},
```

**Step 2: Handle new variants in `devtools.js`**

In `app.js`'s `handleMessage` switch:
```js
case 'spanEvent':
  devtools.addEvent(msg);  // already handled by addEvent — just flows into timeline
  break;

case 'turnMetrics':
  devtools.handleTurnMetrics(msg);
  break;
```

In `devtools.js`:
```js
handleTurnMetrics(data) {
  this.turnLatencies.push(data.latency_ms);
  this.scheduler.markDirty('metrics');
}
```

And update the `_summarize()` method to handle spanEvent:
```js
case 'spanEvent':
  return `${msg.name}  ${msg.status}  ${msg.duration_us > 1000 ? (msg.duration_us / 1000).toFixed(1) + 'ms' : msg.duration_us + 'us'}`;
```

**Step 3: Verify compilation**

Run: `cargo check -p rs-genai-ui`
Expected: clean compile. New variants are defined but not yet sent by any app (the WebSocketSpanLayer will send them in Task 9).

**Step 4: Commit**

```bash
git add apps/adk-web/src/app.rs apps/adk-web/static/js/devtools.js apps/adk-web/static/js/app.js
git commit -m "feat(ui): add SpanEvent and TurnMetrics server message variants"
```

---

### Task 9: Server — WebSocketSpanLayer (OTel Span Bridge)

**Files:**
- Create: `apps/adk-web/src/span_layer.rs`
- Modify: `apps/adk-web/src/main.rs` — add `mod span_layer;`
- Modify: `apps/adk-web/src/bridge.rs` — integrate span layer into session lifecycle
- Modify: `apps/adk-web/Cargo.toml` — add `tracing-subscriber` dependency

**Step 1: Add dependency**

In `apps/adk-web/Cargo.toml`, add:
```toml
tracing-subscriber = { version = "0.3", features = ["registry", "env-filter"] }
```

**Step 2: Create `span_layer.rs`**

```rust
//! WebSocketSpanLayer — bridges tracing spans from rs-genai/rs-adk to the browser.
//!
//! Only captures spans with targets `rs_genai` or `gemini` (the ~13 span types
//! defined in the two spans.rs files). All other tracing output is ignored.
//!
//! Performance: uses a bounded channel (256) to avoid backpressure.
//! Dropped spans don't affect the primary OTLP export path.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::app::ServerMessage;

/// A span event ready to send to the browser.
struct SpanRecord {
    name: String,
    span_id: u64,
    parent_id: Option<u64>,
    start_ns: u64,
    attributes: serde_json::Value,
}

/// Tracing Layer that forwards span close events to a bounded channel.
pub struct WebSocketSpanLayer {
    tx: mpsc::Sender<ServerMessage>,
    spans: Arc<Mutex<HashMap<u64, SpanRecord>>>,
    next_id: AtomicU64,
    epoch: std::time::Instant,
}

impl WebSocketSpanLayer {
    pub fn new(tx: mpsc::Sender<ServerMessage>) -> Self {
        Self {
            tx,
            spans: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            epoch: std::time::Instant::now(),
        }
    }
}

/// Visitor to extract span attributes as JSON.
struct AttrVisitor {
    map: serde_json::Map<String, serde_json::Value>,
}

impl Visit for AttrVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.map.insert(field.name().to_string(), serde_json::Value::String(value.to_string()));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.map.insert(field.name().to_string(), serde_json::json!(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.map.insert(field.name().to_string(), serde_json::json!(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.map.insert(field.name().to_string(), serde_json::json!(value));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.map.insert(field.name().to_string(), serde_json::Value::String(format!("{:?}", value)));
    }
}

impl<S: Subscriber> Layer<S> for WebSocketSpanLayer {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, _ctx: Context<'_, S>) {
        let target = attrs.metadata().target();
        // Only capture rs_genai.* and gemini.agent.* spans
        if !target.starts_with("rs_genai") && !target.starts_with("gemini") {
            return;
        }

        let mut visitor = AttrVisitor { map: serde_json::Map::new() };
        attrs.record(&mut visitor);

        let span_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let record = SpanRecord {
            name: attrs.metadata().name().to_string(),
            span_id,
            parent_id: None, // Could use ctx.current_span() for nesting
            start_ns: self.epoch.elapsed().as_nanos() as u64,
            attributes: serde_json::Value::Object(visitor.map),
        };

        if let Ok(mut spans) = self.spans.lock() {
            spans.insert(id.into_u64(), record);
        }
    }

    fn on_close(&self, id: Id, _ctx: Context<'_, S>) {
        let record = {
            let mut spans = match self.spans.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            spans.remove(&id.into_u64())
        };

        if let Some(record) = record {
            let now_ns = self.epoch.elapsed().as_nanos() as u64;
            let duration_us = (now_ns.saturating_sub(record.start_ns)) / 1000;

            let msg = ServerMessage::SpanEvent {
                name: record.name,
                span_id: format!("{:016x}", record.span_id),
                parent_id: record.parent_id.map(|id| format!("{:016x}", id)),
                duration_us,
                attributes: record.attributes,
                status: "ok".to_string(),
            };

            // Non-blocking send — drop if channel full
            let _ = self.tx.try_send(msg);
        }
    }
}
```

**Step 3: Integrate into `bridge.rs`**

Add a method to SessionBridge that creates a span-forwarding task:

```rust
/// Start forwarding tracing spans to the browser.
/// Returns a receiver that should be polled in a background task.
pub fn start_span_forwarding(&self) -> mpsc::Receiver<ServerMessage> {
    let (span_tx, span_rx) = mpsc::channel(256);
    // The WebSocketSpanLayer is registered per-session.
    // For now, we forward received span messages directly.
    span_rx
}
```

Note: Full integration requires the span layer to be added to the tracing subscriber, which is a global operation. A practical approach is to register the layer globally at startup and use a broadcast channel to fan out to connected browser sessions. This is complex — for the initial implementation, we can start with a simpler approach: the processor's control lane already emits tool call events. We can add span-like events by instrumenting key points in the SessionBridge callbacks.

**Step 4: Verify compilation**

Run: `cargo check -p rs-genai-ui`

**Step 5: Commit**

```bash
git add apps/adk-web/src/span_layer.rs apps/adk-web/src/main.rs apps/adk-web/src/bridge.rs apps/adk-web/Cargo.toml
git commit -m "feat(ui): WebSocketSpanLayer bridges tracing spans to browser timeline"
```

---

### Task 10: OTel Export — Copy as OTLP JSON + Trace ID Badge

**Files:**
- Modify: `apps/adk-web/static/js/devtools.js` — add export button and OTLP JSON serialization
- Modify: `apps/adk-web/static/css/devtools.css` — export button styles

**Step 1: Add export functionality to `devtools.js`**

In the metrics panel rendering, add an export button:

```js
// At the bottom of _renderMetrics():
html += `<div class="metrics-export">
  <button class="export-btn" id="export-otlp-btn">Copy Trace as OTLP JSON</button>
</div>`;

// After innerHTML assignment, wire up the button:
const exportBtn = document.getElementById('export-otlp-btn');
if (exportBtn) {
  exportBtn.addEventListener('click', () => this._exportOtlpJson());
}
```

Implement `_exportOtlpJson()`:

```js
_exportOtlpJson() {
  // Collect all span events from the ring buffer
  const spans = this.events.filter(e => e.type === 'spanEvent');

  // Format as OTLP-compatible JSON
  const otlp = {
    resourceSpans: [{
      resource: {
        attributes: [
          { key: 'service.name', value: { stringValue: 'gemini-rs' } },
          { key: 'session.start', value: { stringValue: new Date(this.sessionStart).toISOString() } },
        ]
      },
      scopeSpans: [{
        scope: { name: 'rs-genai-ui', version: '0.1.0' },
        spans: spans.map(s => ({
          traceId: this._traceId || this._generateTraceId(),
          spanId: s.raw.span_id,
          parentSpanId: s.raw.parent_id || '',
          name: s.raw.name,
          kind: 1, // SPAN_KIND_INTERNAL
          startTimeUnixNano: String((this.sessionStart + s.timeMs) * 1000000),
          endTimeUnixNano: String((this.sessionStart + s.timeMs + (s.raw.duration_us / 1000)) * 1000000),
          attributes: Object.entries(s.raw.attributes || {}).map(([k, v]) => ({
            key: k,
            value: { stringValue: String(v) }
          })),
          status: { code: s.raw.status === 'ok' ? 1 : 2 },
        }))
      }]
    }]
  };

  const json = JSON.stringify(otlp, null, 2);
  navigator.clipboard.writeText(json).then(() => {
    const btn = document.getElementById('export-otlp-btn');
    if (btn) {
      btn.textContent = 'Copied!';
      setTimeout(() => { btn.textContent = 'Copy Trace as OTLP JSON'; }, 1500);
    }
  });
}

_generateTraceId() {
  // Generate a 32-char hex trace ID
  const arr = new Uint8Array(16);
  crypto.getRandomValues(arr);
  return Array.from(arr).map(b => b.toString(16).padStart(2, '0')).join('');
}
```

**Step 2: Add trace ID to status bar**

When a `spanEvent` with name `rs_genai.session` arrives, extract and display the trace ID:

```js
// In addEvent():
if (msg.type === 'spanEvent' && msg.name === 'rs_genai.session') {
  this._traceId = msg.span_id;
  this.scheduler.markDirty('statusBar');
}
```

In `_renderStatusBar()`, if `_traceId` is set, show it as a compact badge:

```js
// After phase display
if (this._traceId) {
  const short = this._traceId.substring(0, 8);
  statusHtml += `<span class="status-separator">|</span>`;
  statusHtml += `<span class="status-trace" title="Trace ID: ${this._traceId}">${short}</span>`;
}
```

**Step 3: Add export CSS**

```css
.metrics-export {
  padding: 8px 0; border-top: 1px solid var(--border-light); margin-top: 4px;
}
.export-btn {
  width: 100%; padding: 8px; border: 1px solid var(--border-light);
  border-radius: 6px; background: #fff; cursor: pointer;
  font-size: 12px; font-weight: 500; color: var(--text-secondary);
  transition: background 0.15s, border-color 0.15s;
}
.export-btn:hover { background: var(--primary-light); border-color: var(--primary); color: var(--primary-dark); }

.status-trace {
  font-family: var(--mono); font-size: 10px; font-weight: 600;
  color: #00695c; background: #e0f7fa; padding: 1px 5px; border-radius: 3px;
  cursor: pointer;
}
.status-trace:hover { background: #b2ebf2; }
```

**Step 4: Test**

- Connect to any app, interact to generate events
- Click "Copy Trace as OTLP JSON" — verify valid JSON in clipboard
- If span events are flowing, verify trace ID appears in status bar
- Paste the JSON and verify it matches OTLP ResourceSpans schema

**Step 5: Commit**

```bash
git add apps/adk-web/static/js/devtools.js apps/adk-web/static/css/devtools.css
git commit -m "feat(ui): OTel export — copy trace as OTLP JSON with trace ID badge"
```

---

## Post-Implementation

After all 10 tasks are complete:

1. **Migrate remaining apps** — Apply SessionBridge to `playbook.rs`, `guardrails.rs`, `support.rs`, `all_config.rs`, `debt_collection.rs`, `call_screening.rs`, `restaurant.rs`, `clinic.rs`.
2. **Performance validation** — Use browser devtools Performance tab to verify timeline append < 0.1ms, minimap repaint < 0.5ms, 60fps scroll.
3. **Clean up old CSS** — Remove unused `.event-entry`, `.nfr-*`, `.events-panel` styles from `devtools.css`.
