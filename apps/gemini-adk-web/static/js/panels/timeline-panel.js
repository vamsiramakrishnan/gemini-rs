/**
 * panels/timeline-panel.js — Chronological event stream with VirtualList.
 *
 * Owns: filter toolbar, virtual list, detail panel, minimap integration.
 * Contract: create(container, scheduler, events) / addEvent(msg) / reset(events) / setMinimap(minimap)
 */
var TimelinePanel = (function () {
  'use strict';

  var U = DevtoolsUtils;

  var BADGE_LABELS = {
    textDelta: 'TEXT', textComplete: 'TEXT', audio: 'AUDIO', turnComplete: 'TURN',
    stateUpdate: 'STATE', phaseChange: 'PHASE', toolCallEvent: 'TOOL', violation: 'VIOL',
    evaluation: 'EVAL', spanEvent: 'SPAN', connected: 'SYS', appMeta: 'META',
    interrupted: 'INT', error: 'ERR', inputTranscription: 'TXIN', outputTranscription: 'TXOUT', thought: 'THINK',
    voiceActivityStart: 'VAD', voiceActivityEnd: 'VAD', telemetry: 'TEL',
    phaseTimeline: 'PHASE', turnMetrics: 'TURN'
  };

  var DEFAULT_HIDDEN = ['audio', 'voiceActivityStart', 'voiceActivityEnd'];

  function TimelinePanel() {
    this._events = null;
    this._scheduler = null;
    this._vl = null;
    this._minimap = null;
    this._container = null;
    this._listContainer = null;
    this._detailPanel = null;
    this._detailContent = null;
    this._expandedIdx = -1;
    this._hiddenTypes = new Set(this._loadFilters());
    this._searchQuery = '';
    this._searchTimer = null;
    this._filteredIndices = null;
    this._searchInput = null;
    this._sessionStart = Date.now();
  }

  TimelinePanel.prototype.create = function (container, scheduler, events) {
    this._container = container;
    this._scheduler = scheduler;
    this._events = events;
    this._sessionStart = Date.now();

    container.className = 'devtools-panel timeline-panel active';

    // Filter toolbar
    var filterBar = U.el('div', 'tl-filters');
    this._buildFilterBar(filterBar);
    container.appendChild(filterBar);

    // Virtual list
    var listContainer = U.el('div', 'timeline-list-container');
    container.appendChild(listContainer);
    this._listContainer = listContainer;

    var self = this;
    this._vl = new VirtualList(listContainer, {
      rowHeight: 28, poolSize: 80,
      render: function (el, event, idx) { self._renderRow(el, event, idx); }
    });

    listContainer.addEventListener('click', function (e) {
      var row = e.target.closest('.tl-row');
      if (!row) return;
      var idx = parseInt(row.dataset.idx, 10);
      if (!isNaN(idx)) self._toggleDetail(idx);
    });

    // Detail panel (fixed below list, not inline)
    var detailPanel = U.el('div', 'tl-detail-panel');
    detailPanel.style.display = 'none';
    var closeBtn = U.el('button', 'tl-detail-close');
    closeBtn.textContent = '\u00d7';
    closeBtn.addEventListener('click', function () {
      detailPanel.style.display = 'none';
      self._expandedIdx = -1;
    });
    detailPanel.appendChild(closeBtn);
    var detailContent = U.el('pre', 'tl-detail-content');
    detailPanel.appendChild(detailContent);
    container.appendChild(detailPanel);
    this._detailPanel = detailPanel;
    this._detailContent = detailContent;

    // VirtualList self-renders via setFilter → _scheduleRender.
    // No scheduler registration needed — avoids double-render flicker.
  };

  TimelinePanel.prototype.setMinimap = function (minimap) {
    this._minimap = minimap;
    var self = this;
    this._listContainer.addEventListener('scroll', function () {
      var ct = self._listContainer;
      var totalH = self._events.length * 28;
      if (totalH <= 0) return;
      minimap.setViewport(
        Math.max(0, Math.min(1, ct.scrollTop / totalH)),
        Math.max(0, Math.min(1, (ct.scrollTop + ct.clientHeight) / totalH))
      );
      self._scheduler.markDirty('minimap');
    }, { passive: true });
  };

  TimelinePanel.prototype.addEvent = function (msg) {
    var elapsed = Date.now() - this._sessionStart;
    var event = {
      type: msg.type,
      time: U.fmtTime(elapsed),
      timeMs: elapsed,
      summary: this._summarize(msg),
      duration: this._extractDuration(msg),
      raw: msg
    };
    this._events.push(event);

    if (this._events.length === 1) this._vl.setItems(this._events);

    this._applyFilters(true);

    this._scheduler.markDirty('minimap');

    return event;
  };

  TimelinePanel.prototype.reset = function (events) {
    this._events = events;
    this._sessionStart = Date.now();
    this._expandedIdx = -1;
    this._detailPanel.style.display = 'none';
    this._filteredIndices = null;
    this._searchQuery = '';
    if (this._searchInput) this._searchInput.value = '';
    this._vl.setItems(events);
    this._vl.setFilter(null);
  };

  TimelinePanel.prototype.getEvents = function () { return this._events; };
  TimelinePanel.prototype.getSessionStart = function () { return this._sessionStart; };

  // --- Filter system ---

  TimelinePanel.prototype._buildFilterBar = function (bar) {
    var self = this;
    var types = [
      'audio', 'voiceActivityStart', 'textDelta', 'textComplete',
      'turnComplete', 'stateUpdate', 'phaseChange', 'toolCallEvent',
      'telemetry', 'evaluation', 'violation', 'interrupted', 'error',
      'inputTranscription', 'outputTranscription', 'thought'
    ];

    types.forEach(function (type) {
      var btn = U.el('button', 'tl-filter-btn');
      if (self._hiddenTypes.has(type)) btn.classList.add('hidden');
      btn.textContent = BADGE_LABELS[type] || type;
      btn.title = type;
      btn.addEventListener('click', function () {
        if (self._hiddenTypes.has(type)) {
          self._hiddenTypes.delete(type);
          if (type === 'voiceActivityStart') self._hiddenTypes.delete('voiceActivityEnd');
          btn.classList.remove('hidden');
        } else {
          self._hiddenTypes.add(type);
          if (type === 'voiceActivityStart') self._hiddenTypes.add('voiceActivityEnd');
          btn.classList.add('hidden');
        }
        self._saveFilters();
        self._applyFilters(false);
      });
      bar.appendChild(btn);
    });

    var search = U.el('input', 'tl-search');
    search.type = 'text';
    search.placeholder = 'Search...';
    search.addEventListener('input', function () {
      clearTimeout(self._searchTimer);
      self._searchTimer = setTimeout(function () {
        self._searchQuery = search.value.trim().toLowerCase();
        self._applyFilters(false);
      }, 150);
    });
    bar.appendChild(search);
    this._searchInput = search;
  };

  TimelinePanel.prototype._applyFilters = function (newEventOnly) {
    var hidden = this._hiddenTypes;
    var query = this._searchQuery;
    var len = this._events.length;

    if (hidden.size === 0 && !query) {
      this._filteredIndices = null;
      this._vl.setFilter(null);
      return;
    }

    // Fast path: single new event, no search query.
    // Only safe when buffer hasn't wrapped (length < capacity).
    var canFastPath = newEventOnly && !query && this._filteredIndices !== null;
    if (canFastPath && this._events._cap && len >= this._events._cap) {
      // Buffer wrapped — filter indices are stale, fall through to full rebuild
      canFastPath = false;
    }
    if (canFastPath) {
      var lastIdx = len - 1;
      var evt = this._events.get(lastIdx);
      if (evt && !hidden.has(evt.type)) this._filteredIndices.push(lastIdx);
      this._vl.setFilter(this._filteredIndices);
      return;
    }

    var indices = [];
    for (var i = 0; i < len; i++) {
      var evt = this._events.get(i);
      if (!evt) continue;
      if (hidden.has(evt.type)) continue;
      if (query && evt.summary.toLowerCase().indexOf(query) === -1) continue;
      indices.push(i);
    }
    this._filteredIndices = indices;
    this._vl.setFilter(indices);
  };

  TimelinePanel.prototype._loadFilters = function () {
    try {
      var stored = localStorage.getItem('devtools-filters');
      if (stored) {
        var parsed = JSON.parse(stored);
        if (!Array.isArray(parsed)) return DEFAULT_HIDDEN.slice();
        // Validate: only keep known filter types. Unknown entries
        // indicate stale localStorage from a previous UI version.
        var known = new Set(Object.keys(BADGE_LABELS));
        var valid = parsed.filter(function (t) { return known.has(t); });
        if (valid.length !== parsed.length) {
          localStorage.removeItem('devtools-filters');
          return DEFAULT_HIDDEN.slice();
        }
        return valid;
      }
    } catch (e) { /* ignore */ }
    return DEFAULT_HIDDEN.slice();
  };

  TimelinePanel.prototype._saveFilters = function () {
    try { localStorage.setItem('devtools-filters', JSON.stringify(Array.from(this._hiddenTypes))); }
    catch (e) { /* ignore */ }
  };

  // --- Row rendering ---

  TimelinePanel.prototype._renderRow = function (el, event, idx) {
    if (!el._tlInit) {
      el.className = 'tl-row';
      el.innerHTML = '';
      var t = U.el('span', 'tl-time'); el.appendChild(t); el._tlTime = t;
      var b = U.el('span', 'tl-badge'); el.appendChild(b); el._tlBadge = b;
      var c = U.el('span', 'tl-content'); el.appendChild(c); el._tlContent = c;
      var d = U.el('span', 'tl-duration'); el.appendChild(d); el._tlDuration = d;
      el._tlInit = true;
    }
    el.dataset.idx = idx;
    el._tlTime.textContent = '[' + event.time + ']';
    el._tlBadge.textContent = BADGE_LABELS[event.type] || event.type.toUpperCase();
    el._tlBadge.className = 'tl-badge tl-badge-' + event.type;
    el._tlContent.textContent = event.summary;
    if (event.duration) {
      el._tlDuration.textContent = event.duration;
      el._tlDuration.style.display = '';
    } else {
      el._tlDuration.textContent = '';
      el._tlDuration.style.display = 'none';
    }
  };

  TimelinePanel.prototype._toggleDetail = function (idx) {
    if (this._expandedIdx === idx) {
      this._detailPanel.style.display = 'none';
      this._expandedIdx = -1;
      return;
    }
    var event = this._events.get(idx);
    if (!event) return;
    try { this._detailContent.textContent = JSON.stringify(event.raw, null, 2); }
    catch (e) { this._detailContent.textContent = String(event.raw); }
    this._detailPanel.style.display = '';
    this._expandedIdx = idx;
  };

  // --- Summarization ---

  TimelinePanel.prototype._summarize = function (msg) {
    switch (msg.type) {
      case 'textDelta': return U.truncText(msg.text, 80);
      case 'textComplete': return U.truncText(msg.text, 80);
      case 'audio': return (msg.data ? msg.data.length : 0) + ' bytes base64';
      case 'turnComplete': return 'Turn complete';
      case 'connected': return 'Session established';
      case 'interrupted': return 'Model interrupted';
      case 'error': return msg.message || 'Unknown error';
      case 'stateUpdate': return msg.key + ' = ' + U.truncText(JSON.stringify(msg.value), 60);
      case 'phaseChange': return (msg.from || '?') + ' -> ' + (msg.to || '?');
      case 'evaluation': return (msg.phase || '') + ': ' + ((msg.score || 0) * 100).toFixed(0) + '%';
      case 'violation': return '[' + (msg.severity || '') + '] ' + (msg.rule || '');
      case 'inputTranscription': return U.truncText(msg.text, 60);
      case 'outputTranscription': return U.truncText(msg.text, 60);
      case 'thought': return U.truncText(msg.text, 60);
      case 'voiceActivityStart': return 'Speech detected';
      case 'voiceActivityEnd': return 'Speech ended';
      case 'appMeta': return msg.info ? msg.info.name : '';
      case 'telemetry': return 'turns: ' + ((msg.stats && msg.stats.turn_count) || 0);
      case 'phaseTimeline': return ((msg.entries || []).length) + ' transitions';
      case 'toolCallEvent': return (msg.name || '') + '(' + U.truncText(msg.args, 30) + ')';
      case 'spanEvent':
        var dur = msg.duration_us > 1000 ? (msg.duration_us / 1000).toFixed(1) + 'ms' : msg.duration_us + 'us';
        return (msg.name || '') + '  ' + (msg.status || '') + '  ' + dur;
      case 'turnMetrics':
        return 'turn ' + msg.turn + '  ' + msg.latency_ms + 'ms  ' + msg.prompt_tokens + '/' + msg.response_tokens + ' tokens';
      default: return U.truncText(JSON.stringify(msg), 80);
    }
  };

  TimelinePanel.prototype._extractDuration = function (msg) {
    if (msg.type === 'telemetry' && msg.stats) {
      var up = msg.stats.uptime_secs;
      if (up) return up.toFixed(1) + 's';
    }
    if (msg.type === 'toolCallEvent' && msg.duration_ms) return msg.duration_ms + 'ms';
    return null;
  };

  return TimelinePanel;
})();
