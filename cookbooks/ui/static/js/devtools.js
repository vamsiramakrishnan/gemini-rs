/**
 * devtools.js — Unified timeline panel with VirtualList rendering
 *
 * Exports:
 *   DevtoolsManager — manages devtools panel state and rendering
 */

var BADGE_LABELS = {
  textDelta: 'TEXT', textComplete: 'TEXT',
  audio: 'AUDIO', turnComplete: 'TURN',
  stateUpdate: 'STATE', phaseChange: 'PHASE',
  toolCallEvent: 'TOOL', violation: 'VIOL',
  evaluation: 'EVAL', spanEvent: 'SPAN',
  connected: 'SYS', appMeta: 'META',
  interrupted: 'INT', error: 'ERR',
  inputTranscription: 'TXIN', outputTranscription: 'TXOUT',
  voiceActivityStart: 'VAD', voiceActivityEnd: 'VAD',
  telemetry: 'TEL', phaseTimeline: 'PHASE',
  turnMetrics: 'TURN'
};

var DEFAULT_HIDDEN_TYPES = ['audio', 'voiceActivityStart', 'voiceActivityEnd'];

class DevtoolsManager {
  /**
   * @param {HTMLElement} container  The .devtools-pane element
   */
  constructor(container) {
    this.container = container;
    this.tabBar = container.querySelector('.devtools-tabs');
    this.contentArea = container.querySelector('.devtools-content');

    // Data storage — bounded ring buffer for events
    this.events = new RingBuffer(10000);
    this.stateData = {};
    this.phases = [];
    this.telemetry = {};
    this.turnLatencies = [];
    this._lastResponseCount = 0;
    this._sparkline = null;
    this.toolCalls = [];
    this.phaseTimeline = [];
    this.sessionStart = Date.now();

    // Current tab
    this.activeTab = 'timeline';

    // Available tabs (updated when appMeta arrives)
    this.availableTabs = ['timeline', 'state', 'metrics'];

    // DOM references
    this.panels = {};
    this.tabButtons = {};

    // Filter system
    this._hiddenTypes = new Set(this._loadFilters());
    this._searchQuery = '';
    this._searchTimer = null;
    this._filteredIndices = null; // null = show all (after type filter)

    // Expanded row tracking
    this._expandedIdx = -1;
    this._detailEl = null;

    // Status bar elements
    this._statusUptimeEl = null;
    this._statusPhaseEl = null;
    this._statusTurnsEl = null;
    this._statusHealthEl = null;
    this._uptimeInterval = null;

    // Render scheduler
    this.scheduler = new RenderScheduler();

    this._initPanels();
    this._initTabs();
    this._initStatusBar();
    this._initResize();
    this._initScheduler();
  }

  _initPanels() {
    var self = this;

    // Timeline panel (new — replaces events)
    var timelinePanel = document.createElement('div');
    timelinePanel.className = 'devtools-panel timeline-panel active';
    timelinePanel.id = 'panel-timeline';

    // Filter toolbar
    var filterBar = document.createElement('div');
    filterBar.className = 'tl-filters';
    this._filterBar = filterBar;
    this._buildFilterBar();
    timelinePanel.appendChild(filterBar);

    // Virtual list container
    var listContainer = document.createElement('div');
    listContainer.className = 'timeline-list-container';
    timelinePanel.appendChild(listContainer);

    this._timelineContainer = listContainer;
    this.panels.timeline = timelinePanel;

    // Initialize VirtualList
    this._timelineVL = new VirtualList(listContainer, {
      rowHeight: 28,
      poolSize: 80,
      render: function (el, event, idx) { self._renderTimelineRow(el, event, idx); }
    });

    // Click handler for expand/collapse
    listContainer.addEventListener('click', function (e) {
      var row = e.target.closest('.tl-row');
      if (!row) return;
      var idx = parseInt(row.dataset.idx, 10);
      if (isNaN(idx)) return;
      self._toggleDetail(idx);
    });

    // State panel (kept as-is for now — Task 5)
    var statePanel = document.createElement('div');
    statePanel.className = 'devtools-panel';
    statePanel.id = 'panel-state';
    statePanel.innerHTML = '<div class="state-empty">No state yet</div>';
    this.panels.state = statePanel;

    // Phases panel (renamed from playbook — Task 6)
    var phasesPanel = document.createElement('div');
    phasesPanel.className = 'devtools-panel playbook-panel';
    phasesPanel.id = 'panel-phases';
    phasesPanel.innerHTML = '<div class="events-empty">No phase changes yet</div>';
    this.panels.phases = phasesPanel;

    // Metrics panel (renamed from NFR — Task 4)
    var metricsPanel = document.createElement('div');
    metricsPanel.className = 'devtools-panel nfr-panel';
    metricsPanel.id = 'panel-metrics';
    metricsPanel.innerHTML = '<div class="events-empty">No metrics yet</div>';
    this.panels.metrics = metricsPanel;

    // Add all to content area
    this.contentArea.appendChild(timelinePanel);
    this.contentArea.appendChild(statePanel);
    this.contentArea.appendChild(phasesPanel);
    this.contentArea.appendChild(metricsPanel);
  }

  _buildFilterBar() {
    var self = this;
    var bar = this._filterBar;
    bar.innerHTML = '';

    // Collect known event types for toggle buttons
    var types = [
      'audio', 'voiceActivityStart', 'textDelta', 'textComplete',
      'turnComplete', 'stateUpdate', 'phaseChange', 'toolCallEvent',
      'telemetry', 'evaluation', 'violation', 'interrupted', 'error',
      'inputTranscription', 'outputTranscription'
    ];

    types.forEach(function (type) {
      var btn = document.createElement('button');
      btn.className = 'tl-filter-btn';
      if (self._hiddenTypes.has(type)) btn.classList.add('hidden');
      btn.textContent = BADGE_LABELS[type] || type;
      btn.title = type;
      btn.addEventListener('click', function () {
        if (self._hiddenTypes.has(type)) {
          self._hiddenTypes.delete(type);
          // Also show voiceActivityEnd when showing VAD start
          if (type === 'voiceActivityStart') self._hiddenTypes.delete('voiceActivityEnd');
          btn.classList.remove('hidden');
        } else {
          self._hiddenTypes.add(type);
          if (type === 'voiceActivityStart') self._hiddenTypes.add('voiceActivityEnd');
          btn.classList.add('hidden');
        }
        self._saveFilters();
        self._applyFilters();
        self.scheduler.markDirty('timeline');
      });
      bar.appendChild(btn);
    });

    // Search input
    var search = document.createElement('input');
    search.type = 'text';
    search.className = 'tl-search';
    search.placeholder = 'Search...';
    search.addEventListener('input', function () {
      clearTimeout(self._searchTimer);
      self._searchTimer = setTimeout(function () {
        self._searchQuery = search.value.trim().toLowerCase();
        self._applyFilters();
        self.scheduler.markDirty('timeline');
      }, 150);
    });
    bar.appendChild(search);
    this._searchInput = search;
  }

  _loadFilters() {
    try {
      var stored = localStorage.getItem('devtools-filters');
      if (stored) return JSON.parse(stored);
    } catch (e) { /* ignore */ }
    return DEFAULT_HIDDEN_TYPES.slice();
  }

  _saveFilters() {
    try {
      localStorage.setItem('devtools-filters', JSON.stringify(Array.from(this._hiddenTypes)));
    } catch (e) { /* ignore */ }
  }

  _applyFilters() {
    var hidden = this._hiddenTypes;
    var query = this._searchQuery;
    var len = this.events.length;

    if (hidden.size === 0 && !query) {
      this._filteredIndices = null;
      this._timelineVL.setFilter(null);
      return;
    }

    var indices = [];
    for (var i = 0; i < len; i++) {
      var evt = this.events.get(i);
      if (!evt) continue;
      if (hidden.has(evt.type)) continue;
      if (query && evt.summary.toLowerCase().indexOf(query) === -1) continue;
      indices.push(i);
    }
    this._filteredIndices = indices;
    this._timelineVL.setFilter(indices);
  }

  _initTabs() {
    this._renderTabs();

    // Collapse button
    var self = this;
    var collapseBtn = this.tabBar.querySelector('.devtools-collapse-btn');
    if (collapseBtn) {
      collapseBtn.addEventListener('click', function () { self.toggleCollapse(); });
    }
  }

  _renderTabs() {
    // Clear existing tab buttons but keep the spacer and collapse btn
    var existing = this.tabBar.querySelectorAll('.devtools-tab');
    existing.forEach(function (t) { t.remove(); });

    var spacer = this.tabBar.querySelector('.devtools-tab-spacer');
    var self = this;

    this.availableTabs.forEach(function (tabId) {
      var btn = document.createElement('button');
      btn.className = 'devtools-tab' + (tabId === self.activeTab ? ' active' : '');
      btn.textContent = self._tabLabel(tabId);
      btn.dataset.tab = tabId;
      btn.addEventListener('click', function () { self.switchTab(tabId); });
      self.tabButtons[tabId] = btn;
      self.tabBar.insertBefore(btn, spacer);
    });
  }

  _tabLabel(tabId) {
    switch (tabId) {
      case 'timeline': return 'Timeline';
      case 'state': return 'State';
      case 'phases': return 'Phases';
      case 'metrics': return 'Metrics';
      default: return tabId;
    }
  }

  _initScheduler() {
    var self = this;
    this.scheduler.register('timeline', function () { self._timelineVL.refresh(); });
    this.scheduler.register('statusBar', function () { self._renderStatusBar(); });
    this.scheduler.register('metrics', function () { self._renderMetrics(); });
  }

  // ------------------------------------------------
  // Timeline row rendering
  // ------------------------------------------------

  _renderTimelineRow(el, event, idx) {
    // Reuse or create child spans
    if (!el._tlInit) {
      el.className = 'tl-row';
      el.innerHTML = '';

      var timeSpan = document.createElement('span');
      timeSpan.className = 'tl-time';
      el.appendChild(timeSpan);
      el._tlTime = timeSpan;

      var badgeSpan = document.createElement('span');
      badgeSpan.className = 'tl-badge';
      el.appendChild(badgeSpan);
      el._tlBadge = badgeSpan;

      var contentSpan = document.createElement('span');
      contentSpan.className = 'tl-content';
      el.appendChild(contentSpan);
      el._tlContent = contentSpan;

      var durationSpan = document.createElement('span');
      durationSpan.className = 'tl-duration';
      el.appendChild(durationSpan);
      el._tlDuration = durationSpan;

      el._tlInit = true;
    }

    el.dataset.idx = idx;
    el._tlTime.textContent = '[' + event.time + ']';

    // Badge
    var label = BADGE_LABELS[event.type] || event.type.toUpperCase();
    el._tlBadge.textContent = label;
    el._tlBadge.className = 'tl-badge tl-badge-' + event.type;

    // Content
    el._tlContent.textContent = event.summary;

    // Duration
    if (event.duration) {
      el._tlDuration.textContent = event.duration;
      el._tlDuration.style.display = '';
    } else {
      el._tlDuration.textContent = '';
      el._tlDuration.style.display = 'none';
    }
  }

  _toggleDetail(idx) {
    // If clicking same row, collapse
    if (this._expandedIdx === idx && this._detailEl) {
      this._detailEl.remove();
      this._detailEl = null;
      this._expandedIdx = -1;
      return;
    }

    // Remove previous detail
    if (this._detailEl) {
      this._detailEl.remove();
      this._detailEl = null;
    }

    var event = this.events.get(idx);
    if (!event) return;

    // Find the clicked row element
    var rows = this._timelineContainer.querySelectorAll('.tl-row[data-idx="' + idx + '"]');
    if (rows.length === 0) return;
    var row = rows[0];

    // Create detail div
    var detail = document.createElement('div');
    detail.className = 'tl-detail';
    try {
      detail.textContent = JSON.stringify(event.raw, null, 2);
    } catch (e) {
      detail.textContent = String(event.raw);
    }

    // Insert after the row in the container
    if (row.nextSibling) {
      this._timelineContainer.insertBefore(detail, row.nextSibling);
    } else {
      this._timelineContainer.appendChild(detail);
    }

    this._detailEl = detail;
    this._expandedIdx = idx;
  }

  // ------------------------------------------------
  // Tab switching
  // ------------------------------------------------

  switchTab(tabId) {
    this.activeTab = tabId;

    Object.entries(this.tabButtons).forEach(function (entry) {
      entry[1].classList.toggle('active', entry[0] === tabId);
    });

    Object.entries(this.panels).forEach(function (entry) {
      entry[1].classList.toggle('active', entry[0] === tabId);
    });
  }

  toggleCollapse() {
    var isCollapsed = this.container.classList.toggle('collapsed');
    var expandBtn = document.querySelector('.devtools-expand-btn');
    if (expandBtn) {
      expandBtn.classList.toggle('visible', isCollapsed);
    }
  }

  expand() {
    this.container.classList.remove('collapsed');
    var expandBtn = document.querySelector('.devtools-expand-btn');
    if (expandBtn) {
      expandBtn.classList.remove('visible');
    }
  }

  // ------------------------------------------------
  // handleAppMeta — configures visible tabs
  // ------------------------------------------------

  handleAppMeta(info) {
    this.availableTabs = ['timeline', 'state'];

    var features = (info.features || []).map(function (f) { return f.toLowerCase(); });

    if (features.includes('state-machine') || info.category === 'advanced' || info.category === 'showcase') {
      this.availableTabs.push('phases');
    }

    // Always show metrics
    this.availableTabs.push('metrics');

    this._renderTabs();

    if (!this.availableTabs.includes(this.activeTab)) {
      this.switchTab('timeline');
    }
  }

  // ------------------------------------------------
  // Reset for new session
  // ------------------------------------------------

  reset() {
    this.events = new RingBuffer(10000);
    this.stateData = {};
    this.phases = [];
    this.telemetry = {};
    this.turnLatencies = [];
    this._lastResponseCount = 0;
    this._sparkline = null;
    this.toolCalls = [];
    this.phaseTimeline = [];
    this.sessionStart = Date.now();
    this._expandedIdx = -1;
    if (this._detailEl) {
      this._detailEl.remove();
      this._detailEl = null;
    }
    this._filteredIndices = null;
    this._searchQuery = '';
    if (this._searchInput) this._searchInput.value = '';

    // Re-bind VirtualList to the new RingBuffer
    this._timelineVL.setItems(this.events);
    this._timelineVL.setFilter(null);

    this.panels.state.innerHTML = '<div class="state-empty">No state yet</div>';
    this.panels.phases.innerHTML = '<div class="events-empty">No phase changes yet</div>';
    this.panels.metrics.innerHTML = '<div class="events-empty">No metrics yet</div>';

    this._stopStatusTicker();
  }

  // ------------------------------------------------
  // Event handlers (API surface — called by app.js)
  // ------------------------------------------------

  addEvent(msg) {
    var elapsed = Date.now() - this.sessionStart;
    var event = {
      type: msg.type,
      time: this._fmtTime(elapsed),
      timeMs: elapsed,
      summary: this._summarize(msg),
      duration: this._extractDuration(msg),
      raw: msg
    };
    this.events.push(event);

    // Re-bind items on first push (VirtualList needs the reference)
    if (this.events.length === 1) {
      this._timelineVL.setItems(this.events);
    }

    this._applyFilters();
    this.scheduler.markDirty('timeline');
  }

  handleStateUpdate(key, value) {
    this.stateData[key] = value;
    this._renderState(key);
  }

  handlePhaseChange(data) {
    this.phases.push(data);
    this._renderPhases();
  }

  handleEvaluation(data) {
    // Evaluations go into the timeline — no separate panel
  }

  handleViolation(data) {
    // Violations go into the timeline — no separate panel
  }

  handleTelemetry(stats) {
    this.telemetry = stats;

    // Track per-turn latencies for sparkline — only when response_count increases
    var rc = stats.response_count || 0;
    if (rc > this._lastResponseCount && stats.last_response_latency_ms > 0) {
      this.turnLatencies.push(stats.last_response_latency_ms);
      this._lastResponseCount = rc;
    }

    // Update status bar from telemetry
    if (stats.current_phase && this._statusPhaseEl) {
      this._statusPhaseEl.textContent = stats.current_phase;
    }
    if (stats.response_count !== undefined && this._statusTurnsEl) {
      this._statusTurnsEl.textContent = stats.response_count;
    }

    // Start the uptime ticker on first telemetry
    if (!this._uptimeInterval) {
      this._startStatusTicker();
    }

    this.scheduler.markDirty('statusBar');
    this.scheduler.markDirty('metrics');
  }

  handlePhaseTimeline(entries) {
    this.phaseTimeline = entries;
    this._renderPhases();
  }

  handleToolCallEvent(data) {
    this.toolCalls.push(data);
    if (!this.telemetry.tool_calls) {
      this.telemetry.tool_calls = 0;
    }
    this.telemetry.tool_calls = this.toolCalls.length;
    this.scheduler.markDirty('metrics');
  }

  // ------------------------------------------------
  // Rendering — State panel (kept for Task 5)
  // ------------------------------------------------

  _renderState(flashKey) {
    var panel = this.panels.state;
    var keys = Object.keys(this.stateData);

    if (keys.length === 0) {
      panel.innerHTML = '<div class="state-empty">No state yet</div>';
      return;
    }

    var groups = {};
    var ungrouped = [];
    keys.forEach(function (key) {
      var colonIdx = key.indexOf(':');
      if (colonIdx > 0 && colonIdx < key.length - 1) {
        var prefix = key.substring(0, colonIdx);
        if (!groups[prefix]) groups[prefix] = [];
        groups[prefix].push(key);
      } else {
        ungrouped.push(key);
      }
    });

    var html = '';
    var self = this;

    if (ungrouped.length > 0) {
      html += this._renderStateGroup(null, ungrouped, flashKey);
    }

    var groupOrder = Object.keys(groups).sort();
    groupOrder.forEach(function (prefix) {
      html += self._renderStateGroup(prefix, groups[prefix].sort(), flashKey);
    });

    panel.innerHTML = html;
    panel.classList.add('state-panel');
  }

  _renderStateGroup(prefix, keys, flashKey) {
    var groupLabel = prefix ? prefix : 'General';
    var groupClass = prefix ? 'state-group-' + prefix : 'state-group-general';
    var self = this;

    var html = '<div class="state-group ' + groupClass + '">';
    if (prefix) {
      html += '<div class="state-group-header">' + this._esc(groupLabel) + '</div>';
    }
    html += '<table class="state-table"><tbody>';

    keys.forEach(function (key) {
      var value = self.stateData[key];
      var fmt = self._formatValue(value);
      var flash = key === flashKey ? ' state-row-flash' : '';
      var displayKey = prefix ? key.substring(prefix.length + 1) : key;
      html += '<tr class="' + flash + '"><td class="state-key">' + self._esc(displayKey) + '</td><td class="state-value ' + fmt.className + '">' + fmt.display + '</td></tr>';
    });

    html += '</tbody></table></div>';
    return html;
  }

  // ------------------------------------------------
  // Rendering — Phases panel (kept for Task 6)
  // ------------------------------------------------

  _renderPhases() {
    var panel = this.panels.phases;
    var hasTimeline = this.phaseTimeline.length > 0;
    var data = hasTimeline ? this.phaseTimeline : this.phases;
    var self = this;

    if (data.length === 0) {
      panel.innerHTML = '<div class="events-empty">No phase changes yet</div>';
      return;
    }

    var html = '';

    if (hasTimeline) {
      html += '<div class="phase-timeline">';
      this.phaseTimeline.forEach(function (entry, i) {
        var durationDisplay = entry.duration_secs < 1
          ? (entry.duration_secs * 1000).toFixed(0) + 'ms'
          : entry.duration_secs.toFixed(1) + 's';
        var triggerLabel = entry.trigger || 'guard';
        var triggerClass = triggerLabel.includes('programmatic') ? 'programmatic' : 'guard';

        html += '<div class="phase-timeline-entry">' +
          '<div class="phase-timeline-left">' +
          '<div class="phase-timeline-dot ' + (i === self.phaseTimeline.length - 1 ? 'current' : '') + '"></div>' +
          (i < self.phaseTimeline.length - 1 ? '<div class="phase-timeline-line"></div>' : '') +
          '</div>' +
          '<div class="phase-timeline-content">' +
          '<div class="phase-timeline-header">' +
          '<span class="phase-name">' + self._esc(entry.from) + '</span>' +
          '<span class="phase-arrow">&rarr;</span>' +
          '<span class="phase-name to">' + self._esc(entry.to) + '</span>' +
          '</div>' +
          '<div class="phase-timeline-meta">' +
          '<span class="phase-trigger ' + triggerClass + '">' + self._esc(triggerLabel) + '</span>' +
          '<span class="phase-duration">' + durationDisplay + '</span>' +
          '<span class="phase-turn">turn ' + entry.turn + '</span>' +
          '</div>' +
          '</div>' +
          '</div>';
      });
      html += '</div>';
    } else {
      this.phases.forEach(function (p) {
        html += '<div class="phase-card">' +
          '<div class="phase-header">' +
          '<span class="phase-name">' + self._esc(p.from) + '</span>' +
          '<span class="phase-arrow">&#8594;</span>' +
          '<span class="phase-name">' + self._esc(p.to) + '</span>' +
          '</div>' +
          '<div class="phase-reason">' + self._esc(p.reason) + '</div>' +
          '</div>';
      });
    }

    panel.innerHTML = html;
    panel.scrollTop = panel.scrollHeight;
  }

  // ------------------------------------------------
  // Rendering — Metrics/NFR panel (kept for Task 4)
  // ------------------------------------------------

  _renderNfr() {
    var panel = this.panels.metrics;
    var stats = this.telemetry;

    if (!stats || Object.keys(stats).length === 0) {
      panel.innerHTML = '<div class="events-empty">No metrics yet</div>';
      return;
    }

    var html = '<div class="nfr-content">';

    if (stats.response_count > 0) {
      var avg = Math.round(stats.avg_response_latency_ms || 0);
      var last = Math.round(stats.last_response_latency_ms || 0);
      var health = avg < 300 ? 'good' : avg < 600 ? 'ok' : 'warn';
      var healthLabel = avg < 300 ? 'Healthy' : avg < 600 ? 'Moderate' : 'Degraded';

      html += '<div class="nfr-hero nfr-hero-' + health + '">' +
        '<div class="nfr-hero-header">' +
        '<span class="nfr-hero-dot"></span>' +
        '<span class="nfr-hero-label">Avg Response Latency</span>' +
        '<span class="nfr-hero-health">' + healthLabel + '</span>' +
        '</div>' +
        '<div class="nfr-hero-value">' + avg + '<span class="nfr-hero-unit">ms</span></div>' +
        '<div class="nfr-hero-sub">' +
        '<span>Last <strong>' + last + 'ms</strong></span>' +
        '<span class="nfr-hero-sep">&middot;</span>' +
        '<span>' + stats.response_count + ' responses</span>' +
        '</div>' +
        '</div>';

      if (stats.response_count > 1) {
        var min = Math.round(stats.min_response_latency_ms || 0);
        var max = Math.round(stats.max_response_latency_ms || 0);
        var range = max - min;
        var lastPct = range > 0 ? Math.min(100, Math.max(0, (last - min) / range * 100)) : 50;
        var avgPct = range > 0 ? Math.min(100, Math.max(0, (avg - min) / range * 100)) : 50;

        html += '<div class="nfr-range-vis">' +
          '<div class="nfr-range-labels"><span>' + min + 'ms</span><span>' + max + 'ms</span></div>' +
          '<div class="nfr-range-track">' +
          '<div class="nfr-range-fill" style="width:100%"></div>' +
          '<div class="nfr-range-marker nfr-range-marker-avg" style="left:' + avgPct + '%" title="avg ' + avg + 'ms"></div>' +
          '<div class="nfr-range-marker nfr-range-marker-last" style="left:' + lastPct + '%" title="last ' + last + 'ms"></div>' +
          '</div>' +
          '<div class="nfr-range-legend">' +
          '<span class="nfr-range-legend-item"><span class="nfr-dot-avg"></span>avg</span>' +
          '<span class="nfr-range-legend-item"><span class="nfr-dot-last"></span>last</span>' +
          '</div>' +
          '</div>';
      }
    }

    html += '<div class="nfr-section">' +
      '<div class="nfr-section-header">' +
      '<span class="nfr-section-icon turn"></span>' +
      '<span class="nfr-section-title">Turn Performance</span>' +
      '</div>' +
      '<div class="nfr-metric-strip">';

    if (stats.avg_turn_duration_ms > 0) {
      var secs = (stats.avg_turn_duration_ms / 1000).toFixed(1);
      html += '<div class="nfr-metric">' +
        '<span class="nfr-metric-value">' + secs + '<span class="nfr-unit">s</span></span>' +
        '<span class="nfr-metric-label">Avg Turn</span>' +
        '</div>';
    }

    html += '<div class="nfr-metric">' +
      '<span class="nfr-metric-value">' + (stats.interruptions || 0) + '</span>' +
      '<span class="nfr-metric-label">Interrupts</span>' +
      '</div>' +
      '</div></div>';

    if (stats.audio_chunks_out > 0) {
      html += '<div class="nfr-section">' +
        '<div class="nfr-section-header">' +
        '<span class="nfr-section-icon audio"></span>' +
        '<span class="nfr-section-title">Audio</span>' +
        '</div>' +
        '<div class="nfr-metric-strip">' +
        '<div class="nfr-metric">' +
        '<span class="nfr-metric-value">' + (stats.audio_kbytes_out || 0) + '<span class="nfr-unit">KB</span></span>' +
        '<span class="nfr-metric-label">Total Out</span>' +
        '</div>' +
        '<div class="nfr-metric">' +
        '<span class="nfr-metric-value">' + (stats.audio_throughput_kbps || 0) + '<span class="nfr-unit">KB/s</span></span>' +
        '<span class="nfr-metric-label">Throughput</span>' +
        '</div>' +
        '<div class="nfr-metric">' +
        '<span class="nfr-metric-value">' + (stats.uptime_secs || 0) + '<span class="nfr-unit">s</span></span>' +
        '<span class="nfr-metric-label">Uptime</span>' +
        '</div>' +
        '</div></div>';
    }

    if (this.toolCalls.length > 0) {
      var self = this;
      html += '<div class="nfr-section">' +
        '<div class="nfr-section-header">' +
        '<span class="nfr-section-icon tools"></span>' +
        '<span class="nfr-section-title">Tool Calls</span>' +
        '<span class="nfr-section-count">' + this.toolCalls.length + '</span>' +
        '</div>' +
        '<div class="nfr-tool-list">';

      this.toolCalls.slice(-5).forEach(function (tc) {
        html += '<div class="nfr-tool-entry">' +
          '<span class="nfr-tool-name">' + self._esc(tc.name) + '</span>' +
          '<span class="nfr-tool-args">' + self._truncate(tc.args, 60) + '</span>' +
          (tc.result ? '<span class="nfr-tool-result">' + self._truncate(tc.result, 80) + '</span>' : '') +
          '</div>';
      });

      html += '</div></div>';
    }

    html += '</div>';
    panel.innerHTML = html;

    this._updateHealthIndicator(stats);
  }

  // ------------------------------------------------
  // Status bar
  // ------------------------------------------------

  _initStatusBar() {
    this._statusUptimeEl = document.getElementById('status-uptime');
    this._statusPhaseEl = document.getElementById('status-phase');
    this._statusTurnsEl = document.getElementById('status-turns');
    this._statusHealthEl = document.getElementById('status-health');
  }

  _renderStatusBar() {
    // Called by scheduler — updates uptime from telemetry
    if (this._statusUptimeEl) {
      var elapsed = Date.now() - this.sessionStart;
      this._statusUptimeEl.textContent = this._fmtTime(elapsed);
    }
  }

  _startStatusTicker() {
    var self = this;
    // Update every second via setInterval (not rAF — it's just text)
    this._uptimeInterval = setInterval(function () {
      self.scheduler.markDirty('statusBar');
    }, 1000);
  }

  _stopStatusTicker() {
    if (this._uptimeInterval) {
      clearInterval(this._uptimeInterval);
      this._uptimeInterval = null;
    }
    if (this._statusUptimeEl) this._statusUptimeEl.textContent = '--';
    if (this._statusPhaseEl) this._statusPhaseEl.textContent = '--';
    if (this._statusTurnsEl) this._statusTurnsEl.textContent = '0';
    if (this._statusHealthEl) this._statusHealthEl.className = 'status-health-dot';
  }

  _updateHealthIndicator(stats) {
    if (!this._statusHealthEl) return;
    var avg = stats.avg_response_latency_ms || 0;
    if (stats.response_count > 0) {
      var cls = avg < 300 ? 'good' : avg < 600 ? 'ok' : 'warn';
      this._statusHealthEl.className = 'status-health-dot ' + cls;
    }
  }

  _initResize() {
    var self = this;
    var handle = document.getElementById('devtools-resize-handle');
    if (!handle) return;

    var startX, startWidth;

    var onMouseMove = function (e) {
      var dx = startX - e.clientX;
      var newWidth = Math.min(520, Math.max(280, startWidth + dx));
      self.container.style.width = newWidth + 'px';
      self.container.style.minWidth = newWidth + 'px';
      e.preventDefault();
    };

    var onMouseUp = function () {
      handle.classList.remove('active');
      document.removeEventListener('mousemove', onMouseMove);
      document.removeEventListener('mouseup', onMouseUp);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    };

    handle.addEventListener('mousedown', function (e) {
      startX = e.clientX;
      startWidth = self.container.offsetWidth;
      handle.classList.add('active');
      document.body.style.cursor = 'col-resize';
      document.body.style.userSelect = 'none';
      document.addEventListener('mousemove', onMouseMove);
      document.addEventListener('mouseup', onMouseUp);
      e.preventDefault();
    });
  }

  // ------------------------------------------------
  // Helpers
  // ------------------------------------------------

  _fmtTime(ms) {
    var totalSec = ms / 1000;
    if (totalSec < 60) {
      return 'T+' + totalSec.toFixed(1) + 's';
    }
    var min = Math.floor(totalSec / 60);
    var sec = (totalSec % 60).toFixed(0).padStart(2, '0');
    return 'T+' + min + ':' + sec;
  }

  _summarize(msg) {
    switch (msg.type) {
      case 'textDelta':
        return this._truncText(msg.text, 80);
      case 'textComplete':
        return this._truncText(msg.text, 80);
      case 'audio':
        var len = msg.data ? msg.data.length : 0;
        return len + ' bytes base64';
      case 'turnComplete':
        return 'Turn complete';
      case 'connected':
        return 'Session established';
      case 'interrupted':
        return 'Model interrupted';
      case 'error':
        return msg.message || 'Unknown error';
      case 'stateUpdate':
        return msg.key + ' = ' + this._truncText(JSON.stringify(msg.value), 60);
      case 'phaseChange':
        return (msg.from || '?') + ' -> ' + (msg.to || '?');
      case 'evaluation':
        return (msg.phase || '') + ': ' + ((msg.score || 0) * 100).toFixed(0) + '%';
      case 'violation':
        return '[' + (msg.severity || '') + '] ' + (msg.rule || '');
      case 'inputTranscription':
        return this._truncText(msg.text, 60);
      case 'outputTranscription':
        return this._truncText(msg.text, 60);
      case 'voiceActivityStart':
        return 'Speech detected';
      case 'voiceActivityEnd':
        return 'Speech ended';
      case 'appMeta':
        return msg.info ? msg.info.name : '';
      case 'telemetry':
        return 'turns: ' + ((msg.stats && msg.stats.turn_count) || 0);
      case 'phaseTimeline':
        return ((msg.entries || []).length) + ' transitions';
      case 'toolCallEvent':
        return (msg.name || '') + '(' + this._truncText(msg.args, 30) + ')';
      default:
        return this._truncText(JSON.stringify(msg), 80);
    }
  }

  _extractDuration(msg) {
    if (msg.type === 'telemetry' && msg.stats) {
      var up = msg.stats.uptime_secs;
      if (up) return up.toFixed(1) + 's';
    }
    if (msg.type === 'toolCallEvent' && msg.duration_ms) {
      return msg.duration_ms + 'ms';
    }
    return null;
  }

  _truncText(str, max) {
    if (str === null || str === undefined) return '';
    var s = String(str);
    if (s.length <= max) return s;
    return s.substring(0, max) + '...';
  }

  _truncate(str, max) {
    var escaped = this._esc(String(str));
    if (escaped.length <= max) return escaped;
    return escaped.substring(0, max) + '<span class="truncated">...</span>';
  }

  _formatValue(value) {
    if (value === null || value === undefined) {
      return { display: 'null', className: 'null' };
    }
    if (typeof value === 'string') {
      return { display: '"' + this._esc(value) + '"', className: 'string' };
    }
    if (typeof value === 'number') {
      return { display: String(value), className: 'number' };
    }
    if (typeof value === 'boolean') {
      return { display: String(value), className: 'boolean' };
    }
    var json = JSON.stringify(value, null, 1);
    var truncated = json.length > 120 ? json.substring(0, 120) + '...' : json;
    return { display: this._esc(truncated), className: '' };
  }

  _esc(str) {
    var div = document.createElement('div');
    div.textContent = str;
    return div.innerHTML;
  }
}
