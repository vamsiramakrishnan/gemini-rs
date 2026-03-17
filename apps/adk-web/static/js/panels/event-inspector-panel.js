/**
 * panels/event-inspector-panel.js — Per-event JSON detail inspector.
 *
 * Displays individual events with full JSON detail, syntax highlighting,
 * and searchable/filterable event list. Supports inline trace correlation.
 *
 * Contract: create(container) / addEvent(event) / reset()
 */
var EventInspectorPanel = (function () {
  'use strict';

  var U = DevtoolsUtils;

  var EVENT_BADGES = {
    'audio':            { label: 'Audio',      cls: 'evt-audio' },
    'text':             { label: 'Text',       cls: 'evt-text' },
    'toolCall':         { label: 'Tool Call',  cls: 'evt-tool' },
    'toolResponse':     { label: 'Tool Resp',  cls: 'evt-tool' },
    'functionCall':     { label: 'Fn Call',    cls: 'evt-tool' },
    'functionResponse': { label: 'Fn Resp',    cls: 'evt-tool' },
    'stateUpdate':      { label: 'State',      cls: 'evt-state' },
    'phaseChange':      { label: 'Phase',      cls: 'evt-phase' },
    'spanEvent':        { label: 'Span',       cls: 'evt-span' },
    'error':            { label: 'Error',      cls: 'evt-error' },
    'turnComplete':     { label: 'Turn End',   cls: 'evt-turn' },
    'interrupted':      { label: 'Interrupt',  cls: 'evt-interrupt' },
    'setupComplete':    { label: 'Setup',      cls: 'evt-setup' }
  };

  function EventInspectorPanel() {
    this._container = null;
    this._events = [];
    this._filteredEvents = [];
    this._searchTerm = '';
    this._typeFilter = 'all';
    this._listEl = null;
    this._detailEl = null;
    this._selectedIdx = -1;
    this._countEl = null;
    this._sessionStart = Date.now();
  }

  EventInspectorPanel.prototype.create = function (container) {
    this._container = container;
    container.className = 'devtools-panel event-inspector-panel';
    this._build();
  };

  EventInspectorPanel.prototype._build = function () {
    var c = this._container;
    c.innerHTML = '';

    // Toolbar
    var toolbar = U.el('div', 'evt-toolbar');

    var search = U.el('input', 'evt-search');
    search.type = 'text';
    search.placeholder = 'Search events...';
    var self = this;
    search.addEventListener('input', function () {
      self._searchTerm = this.value.toLowerCase();
      self._applyFilter();
    });
    toolbar.appendChild(search);

    var typeSelect = U.el('select', 'evt-type-filter');
    var allOpt = U.el('option', '');
    allOpt.value = 'all';
    allOpt.textContent = 'All Types';
    typeSelect.appendChild(allOpt);

    Object.keys(EVENT_BADGES).forEach(function (type) {
      var opt = U.el('option', '');
      opt.value = type;
      opt.textContent = EVENT_BADGES[type].label;
      typeSelect.appendChild(opt);
    });

    typeSelect.addEventListener('change', function () {
      self._typeFilter = this.value;
      self._applyFilter();
    });
    toolbar.appendChild(typeSelect);

    this._countEl = U.el('span', 'evt-count');
    this._countEl.textContent = '0 events';
    toolbar.appendChild(this._countEl);

    c.appendChild(toolbar);

    // Split view
    var split = U.el('div', 'evt-split');

    var listWrap = U.el('div', 'evt-list-wrap');
    this._listEl = U.el('div', 'evt-list');
    listWrap.appendChild(this._listEl);
    split.appendChild(listWrap);

    this._detailEl = U.el('div', 'evt-detail');
    this._detailEl.innerHTML = '<div class="evt-detail-empty">Select an event to inspect</div>';
    split.appendChild(this._detailEl);

    c.appendChild(split);
  };

  EventInspectorPanel.prototype.addEvent = function (event) {
    var evt = {
      idx: this._events.length,
      type: event.type || 'unknown',
      timestamp: event.timestamp || Date.now(),
      data: event,
      searchText: JSON.stringify(event).toLowerCase()
    };
    this._events.push(evt);
    this._applyFilter();
  };

  EventInspectorPanel.prototype._applyFilter = function () {
    var self = this;
    this._filteredEvents = this._events.filter(function (evt) {
      if (self._typeFilter !== 'all' && evt.type !== self._typeFilter) return false;
      if (self._searchTerm && evt.searchText.indexOf(self._searchTerm) === -1) return false;
      return true;
    });

    if (this._countEl) {
      this._countEl.textContent = this._filteredEvents.length + ' / ' + this._events.length + ' events';
    }

    this._renderList();
  };

  EventInspectorPanel.prototype._renderList = function () {
    if (!this._listEl) return;
    this._listEl.innerHTML = '';

    var self = this;

    if (this._filteredEvents.length === 0) {
      var empty = U.el('div', 'evt-list-empty');
      empty.textContent = this._events.length === 0 ? 'No events yet' : 'No matching events';
      this._listEl.appendChild(empty);
      return;
    }

    // Only render last 500 for performance
    var start = Math.max(0, this._filteredEvents.length - 500);
    for (var i = start; i < this._filteredEvents.length; i++) {
      var evt = this._filteredEvents[i];
      var row = U.el('div', 'evt-row' + (evt.idx === self._selectedIdx ? ' selected' : ''));

      // Timestamp
      var timeEl = U.el('span', 'evt-time');
      var elapsed = evt.timestamp - self._sessionStart;
      timeEl.textContent = U.fmtTime(elapsed >= 0 ? elapsed : 0);
      row.appendChild(timeEl);

      // Type badge
      var badgeInfo = EVENT_BADGES[evt.type] || { label: evt.type, cls: 'evt-default' };
      var badge = U.el('span', 'evt-badge ' + badgeInfo.cls);
      badge.textContent = badgeInfo.label;
      row.appendChild(badge);

      // Preview
      var preview = U.el('span', 'evt-preview');
      preview.textContent = U.truncText(_summarize(evt.data), 50);
      row.appendChild(preview);

      (function (e) {
        row.addEventListener('click', function () {
          self._selectedIdx = e.idx;
          self._renderList();
          self._renderDetail(e);
        });
      })(evt);

      this._listEl.appendChild(row);
    }

    // Auto-scroll to bottom for new events
    this._listEl.scrollTop = this._listEl.scrollHeight;
  };

  EventInspectorPanel.prototype._renderDetail = function (evt) {
    var d = this._detailEl;
    d.innerHTML = '';

    // Header
    var header = U.el('div', 'evt-detail-header');

    var badgeInfo = EVENT_BADGES[evt.type] || { label: evt.type, cls: 'evt-default' };
    var badge = U.el('span', 'evt-badge-lg ' + badgeInfo.cls);
    badge.textContent = badgeInfo.label;
    header.appendChild(badge);

    var timeEl = U.el('span', 'evt-detail-time');
    timeEl.textContent = '#' + evt.idx + ' at ' + U.fmtTime(evt.timestamp - this._sessionStart);
    header.appendChild(timeEl);

    d.appendChild(header);

    // Trace correlation
    if (evt.data.span_id || evt.data.trace_id) {
      var traceEl = U.el('div', 'evt-trace-link');
      traceEl.textContent = 'Trace: ' + (evt.data.trace_id || evt.data.span_id || '').substring(0, 16);
      traceEl.title = 'Span ID: ' + (evt.data.span_id || 'N/A');
      d.appendChild(traceEl);
    }

    // Key-value summary for common fields
    var summaryFields = _extractSummaryFields(evt.data);
    if (summaryFields.length > 0) {
      var table = U.el('table', 'evt-detail-table');
      summaryFields.forEach(function (pair) {
        var tr = U.el('tr', '');
        var th = U.el('th', '');
        th.textContent = pair[0];
        tr.appendChild(th);
        var td = U.el('td', '');
        td.textContent = U.truncText(String(pair[1]), 80);
        tr.appendChild(td);
        table.appendChild(tr);
      });
      d.appendChild(table);
    }

    // Full JSON
    var jsonTitle = U.el('div', 'evt-detail-subtitle');
    jsonTitle.textContent = 'Raw JSON';
    d.appendChild(jsonTitle);

    var pre = U.el('pre', 'evt-detail-json');
    pre.textContent = JSON.stringify(evt.data, null, 2);
    d.appendChild(pre);

    // Copy button
    var copyBtn = U.el('button', 'evt-copy-btn');
    copyBtn.textContent = 'Copy JSON';
    copyBtn.addEventListener('click', function () {
      navigator.clipboard.writeText(JSON.stringify(evt.data, null, 2)).then(function () {
        copyBtn.textContent = 'Copied!';
        setTimeout(function () { copyBtn.textContent = 'Copy JSON'; }, 1500);
      });
    });
    d.appendChild(copyBtn);
  };

  EventInspectorPanel.prototype.setSessionStart = function (ts) {
    this._sessionStart = ts;
  };

  EventInspectorPanel.prototype.reset = function () {
    this._events = [];
    this._filteredEvents = [];
    this._searchTerm = '';
    this._typeFilter = 'all';
    this._selectedIdx = -1;
    this._sessionStart = Date.now();
    if (this._listEl) this._listEl.innerHTML = '';
    if (this._detailEl) {
      this._detailEl.innerHTML = '<div class="evt-detail-empty">Select an event to inspect</div>';
    }
    if (this._countEl) this._countEl.textContent = '0 events';
  };

  function _summarize(data) {
    if (data.text) return data.text;
    if (data.name) return data.name;
    if (data.key) return data.key + ' = ' + JSON.stringify(data.value);
    if (data.phase || data.to) return (data.from || '?') + ' -> ' + (data.to || data.phase);
    if (data.message) return data.message;
    return '';
  }

  function _extractSummaryFields(data) {
    var fields = [];
    var keys = ['type', 'name', 'text', 'key', 'value', 'phase', 'from', 'to',
                'tool_name', 'function_name', 'status', 'duration_ms', 'error'];
    keys.forEach(function (k) {
      if (data[k] !== undefined) {
        var v = data[k];
        if (typeof v === 'object') v = JSON.stringify(v);
        fields.push([k, v]);
      }
    });
    return fields;
  }

  return EventInspectorPanel;
})();
