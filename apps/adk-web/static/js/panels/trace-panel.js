/**
 * panels/trace-panel.js — Trace waterfall / flame chart visualization.
 *
 * Displays spans as horizontal bars in a waterfall layout, showing
 * parent-child relationships, duration, and timing relative to the
 * root span. Supports zoom/pan and click-to-inspect.
 *
 * Contract: create(container) / addSpan(span) / reset()
 */
var TracePanel = (function () {
  'use strict';

  var U = DevtoolsUtils;

  var SPAN_COLORS = {
    'invocation':   '#4285f4',
    'agent_run':    '#34a853',
    'call_llm':     '#fbbc04',
    'execute_tool': '#ea4335',
    'model_call':   '#ab47bc',
    'session':      '#00acc1',
    'default':      '#78909c'
  };

  function TracePanel() {
    this._spans = [];           // All spans received
    this._spanMap = {};         // spanId -> span
    this._rootStart = null;     // Earliest span start timestamp
    this._container = null;
    this._waterfall = null;
    this._detail = null;
    this._selectedSpan = null;
    this._zoomLevel = 1;
    this._scrollOffset = 0;
  }

  TracePanel.prototype.create = function (container) {
    this._container = container;
    container.className = 'devtools-panel trace-panel';
    this._build();
  };

  TracePanel.prototype._build = function () {
    var c = this._container;
    c.innerHTML = '';

    // Header with controls
    var header = U.el('div', 'trace-header');

    var title = U.el('span', 'trace-title');
    title.textContent = 'Trace Waterfall';
    header.appendChild(title);

    var controls = U.el('div', 'trace-controls');

    var zoomIn = U.el('button', 'trace-btn');
    zoomIn.textContent = '+';
    zoomIn.title = 'Zoom in';
    var self = this;
    zoomIn.addEventListener('click', function () { self._zoom(1.5); });
    controls.appendChild(zoomIn);

    var zoomOut = U.el('button', 'trace-btn');
    zoomOut.textContent = '-';
    zoomOut.title = 'Zoom out';
    zoomOut.addEventListener('click', function () { self._zoom(0.67); });
    controls.appendChild(zoomOut);

    var resetBtn = U.el('button', 'trace-btn');
    resetBtn.textContent = 'Fit';
    resetBtn.title = 'Reset zoom';
    resetBtn.addEventListener('click', function () { self._zoomLevel = 1; self._render(); });
    controls.appendChild(resetBtn);

    header.appendChild(controls);
    c.appendChild(header);

    // Split: waterfall left, detail right
    var split = U.el('div', 'trace-split');

    var waterfallWrap = U.el('div', 'trace-waterfall-wrap');
    this._waterfall = U.el('div', 'trace-waterfall');
    waterfallWrap.appendChild(this._waterfall);
    split.appendChild(waterfallWrap);

    this._detail = U.el('div', 'trace-detail');
    this._detail.innerHTML = '<div class="trace-detail-empty">Click a span to inspect</div>';
    split.appendChild(this._detail);

    c.appendChild(split);

    // Legend
    var legend = U.el('div', 'trace-legend');
    Object.keys(SPAN_COLORS).forEach(function (key) {
      if (key === 'default') return;
      var item = U.el('span', 'trace-legend-item');
      var swatch = U.el('span', 'trace-swatch');
      swatch.style.backgroundColor = SPAN_COLORS[key];
      item.appendChild(swatch);
      var label = document.createTextNode(' ' + key);
      item.appendChild(label);
      legend.appendChild(item);
    });
    c.appendChild(legend);
  };

  TracePanel.prototype.addSpan = function (span) {
    // Normalize span data
    var s = {
      id: span.span_id || span.id || String(this._spans.length),
      parentId: span.parent_id || null,
      name: span.name || 'unknown',
      startTime: span.start_time || span.timestamp || Date.now(),
      duration: span.duration_ms || span.duration || 0,
      status: span.status || 'ok',
      attributes: span.attributes || {},
      category: _classifySpan(span.name || '')
    };

    if (this._rootStart === null || s.startTime < this._rootStart) {
      this._rootStart = s.startTime;
    }

    this._spans.push(s);
    this._spanMap[s.id] = s;
    this._render();
  };

  TracePanel.prototype._zoom = function (factor) {
    this._zoomLevel = Math.max(0.1, Math.min(20, this._zoomLevel * factor));
    this._render();
  };

  TracePanel.prototype._render = function () {
    if (!this._waterfall || this._spans.length === 0) return;

    var w = this._waterfall;
    w.innerHTML = '';

    var totalDuration = 0;
    var self = this;

    // Calculate total duration
    this._spans.forEach(function (s) {
      var end = (s.startTime - self._rootStart) + s.duration;
      if (end > totalDuration) totalDuration = end;
    });
    if (totalDuration === 0) totalDuration = 1;

    var containerWidth = w.offsetWidth || 400;

    // Build rows sorted by start time
    var sorted = this._spans.slice().sort(function (a, b) {
      return a.startTime - b.startTime;
    });

    // Calculate depth (indent level)
    sorted.forEach(function (s) {
      s._depth = 0;
      if (s.parentId && self._spanMap[s.parentId]) {
        s._depth = (self._spanMap[s.parentId]._depth || 0) + 1;
      }
    });

    sorted.forEach(function (s) {
      var row = U.el('div', 'trace-row' + (s.id === (self._selectedSpan || {}).id ? ' selected' : ''));

      // Label column
      var label = U.el('div', 'trace-label');
      label.style.paddingLeft = (s._depth * 16 + 4) + 'px';
      label.textContent = U.truncText(s.name, 28);
      label.title = s.name;
      row.appendChild(label);

      // Bar column
      var barCol = U.el('div', 'trace-bar-col');
      var bar = U.el('div', 'trace-bar');

      var offset = (s.startTime - self._rootStart) / totalDuration;
      var width = s.duration / totalDuration;

      bar.style.left = (offset * 100 * self._zoomLevel) + '%';
      bar.style.width = Math.max(2, width * containerWidth * self._zoomLevel) + 'px';
      bar.style.backgroundColor = SPAN_COLORS[s.category] || SPAN_COLORS['default'];

      if (s.status === 'error') {
        bar.classList.add('trace-bar-error');
      }

      // Duration label on bar
      var durLabel = U.el('span', 'trace-bar-dur');
      durLabel.textContent = s.duration < 1 ? '<1ms' : s.duration.toFixed(0) + 'ms';
      bar.appendChild(durLabel);

      barCol.appendChild(bar);
      row.appendChild(barCol);

      row.addEventListener('click', function () { self._selectSpan(s); });
      w.appendChild(row);
    });
  };

  TracePanel.prototype._selectSpan = function (span) {
    this._selectedSpan = span;
    this._render();
    this._renderDetail(span);
  };

  TracePanel.prototype._renderDetail = function (s) {
    var d = this._detail;
    d.innerHTML = '';

    var title = U.el('div', 'trace-detail-title');
    title.textContent = s.name;
    d.appendChild(title);

    var info = [
      ['Span ID', s.id],
      ['Parent ID', s.parentId || '(root)'],
      ['Duration', s.duration.toFixed(1) + 'ms'],
      ['Status', s.status],
      ['Category', s.category],
      ['Offset', ((s.startTime - this._rootStart) || 0).toFixed(1) + 'ms']
    ];

    var table = U.el('table', 'trace-detail-table');
    info.forEach(function (pair) {
      var tr = U.el('tr', '');
      var th = U.el('th', '');
      th.textContent = pair[0];
      tr.appendChild(th);
      var td = U.el('td', '');
      td.textContent = pair[1];
      tr.appendChild(td);
      table.appendChild(tr);
    });
    d.appendChild(table);

    // Attributes
    if (Object.keys(s.attributes).length > 0) {
      var attrTitle = U.el('div', 'trace-detail-subtitle');
      attrTitle.textContent = 'Attributes';
      d.appendChild(attrTitle);

      var pre = U.el('pre', 'trace-detail-json');
      pre.textContent = JSON.stringify(s.attributes, null, 2);
      d.appendChild(pre);
    }
  };

  TracePanel.prototype.reset = function () {
    this._spans = [];
    this._spanMap = {};
    this._rootStart = null;
    this._selectedSpan = null;
    this._zoomLevel = 1;
    if (this._waterfall) this._waterfall.innerHTML = '';
    if (this._detail) {
      this._detail.innerHTML = '<div class="trace-detail-empty">Click a span to inspect</div>';
    }
  };

  function _classifySpan(name) {
    if (name.indexOf('invocation') !== -1) return 'invocation';
    if (name.indexOf('agent_run') !== -1) return 'agent_run';
    if (name.indexOf('call_llm') !== -1 || name.indexOf('model_call') !== -1) return 'call_llm';
    if (name.indexOf('execute_tool') !== -1 || name.indexOf('tool_') !== -1) return 'execute_tool';
    if (name.indexOf('session') !== -1) return 'session';
    return 'default';
  }

  return TracePanel;
})();
