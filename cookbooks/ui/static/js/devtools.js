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

    // State panel tracking (Task 5 — targeted updates)
    this._stateMap = new Map();       // key -> { keyEl, valueEl, row, group }
    this._stateGroups = {};           // prefix -> { header, tbody, container, collapsed }
    this._stateSearchTerm = '';
    this._stateGroupContainer = null;

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

    // State panel (Task 5 — targeted updates with diff flash)
    var statePanel = document.createElement('div');
    statePanel.className = 'devtools-panel';
    statePanel.id = 'panel-state';
    this.panels.state = statePanel;
    this._initStatePanel();

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

    // Minimap
    var minimapCanvas = document.getElementById('minimap-canvas');
    if (minimapCanvas) {
      this._minimap = new Minimap(minimapCanvas, {
        onClick: function (ratio) {
          var container = self._timelineContainer;
          var totalH = self.events.length * 28;
          container.scrollTop = ratio * totalH;
        }
      });
      this._minimap.setEvents(this.events);
      this.scheduler.register('minimap', function () {
        self._minimap.setSessionDuration(Date.now() - self.sessionStart);
        self._minimap.render();
      });

      // Track timeline scroll position for viewport overlay
      this._timelineContainer.addEventListener('scroll', function () {
        var ct = self._timelineContainer;
        var totalH = self.events.length * 28;
        if (totalH <= 0) return;
        var startRatio = ct.scrollTop / totalH;
        var endRatio = (ct.scrollTop + ct.clientHeight) / totalH;
        self._minimap.setViewport(
          Math.max(0, Math.min(1, startRatio)),
          Math.max(0, Math.min(1, endRatio))
        );
        self.scheduler.markDirty('minimap');
      }, { passive: true });
    }
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

    // Re-bind VirtualList and minimap to the new RingBuffer
    this._timelineVL.setItems(this.events);
    this._timelineVL.setFilter(null);
    if (this._minimap) {
      this._minimap.setEvents(this.events);
      this._minimap.setViewport(0, 1);
      this.scheduler.markDirty('minimap');
    }

    // Reset state panel tracking (Task 5)
    this._stateMap = new Map();
    this._stateGroups = {};
    this._stateSearchTerm = '';
    this._initStatePanel();

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
    this.scheduler.markDirty('minimap');
  }

  handleStateUpdate(key, value) {
    var self = this;
    var prev = this.stateData[key];
    this.stateData[key] = value;

    if (this._stateMap.has(key)) {
      // Update existing cell — no DOM creation
      var entry = this._stateMap.get(key);
      var fmt = this._formatValue(value);
      entry.valueEl.textContent = '';
      entry.valueEl.textContent = fmt.display;
      entry.valueEl.className = 'state-value ' + fmt.className;

      // Flash animation
      entry.row.classList.remove('state-row-flash');
      void entry.row.offsetWidth; // force reflow for re-animation
      entry.row.classList.add('state-row-flash');

      // Show previous value for 2 seconds
      if (prev !== undefined) {
        var prevStr = typeof prev === 'string' ? '"' + prev + '"' : JSON.stringify(prev);
        var wasSpan = document.createElement('span');
        wasSpan.className = 'state-was';
        wasSpan.textContent = ' was: ' + self._truncText(prevStr, 30);
        entry.valueEl.appendChild(wasSpan);
        setTimeout(function () { if (wasSpan.parentNode) wasSpan.remove(); }, 2000);
      }
    } else {
      // New key — create row and add to appropriate group
      this._addStateRow(key, value);
    }

    // Apply search filter if active
    if (this._stateSearchTerm) this._filterState(this._stateSearchTerm);
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

  handleTurnMetrics(data) {
    this.turnLatencies.push(data.latency_ms);
    this.scheduler.markDirty('metrics');
  }

  // ------------------------------------------------
  // Rendering — State panel (Task 5 — targeted updates)
  // ------------------------------------------------

  _initStatePanel() {
    var panel = this.panels.state;
    panel.innerHTML = '';
    panel.classList.add('state-panel');

    var self = this;

    // Search bar
    var search = document.createElement('input');
    search.className = 'state-search';
    search.placeholder = 'Filter keys...';
    search.addEventListener('input', function () {
      self._stateSearchTerm = search.value.toLowerCase();
      self._filterState(self._stateSearchTerm);
    });
    panel.appendChild(search);

    // Scrollable container for groups
    var groupContainer = document.createElement('div');
    groupContainer.className = 'state-groups';
    panel.appendChild(groupContainer);
    this._stateGroupContainer = groupContainer;
  }

  _createStateGroup(groupKey, prefix) {
    var container = document.createElement('div');
    container.className = 'state-group collapsed'; // start collapsed

    var header = document.createElement('div');
    header.className = 'state-group-header';
    header.textContent = prefix || 'General';
    header.addEventListener('click', function () {
      container.classList.toggle('collapsed');
    });
    container.appendChild(header);

    var table = document.createElement('table');
    table.className = 'state-table';
    var tbody = document.createElement('tbody');
    table.appendChild(tbody);
    container.appendChild(table);

    this._stateGroupContainer.appendChild(container);
    this._stateGroups[groupKey] = { header: header, tbody: tbody, container: container, collapsed: true };

    // General group starts expanded
    if (!prefix) container.classList.remove('collapsed');
  }

  _addStateRow(key, value) {
    var colonIdx = key.indexOf(':');
    var prefix = (colonIdx > 0 && colonIdx < key.length - 1) ? key.substring(0, colonIdx) : null;
    var displayKey = prefix ? key.substring(colonIdx + 1) : key;

    var groupKey = prefix || '_general';
    if (!this._stateGroups[groupKey]) {
      this._createStateGroup(groupKey, prefix);
    }
    var group = this._stateGroups[groupKey];

    var row = document.createElement('tr');
    row.className = 'state-row';
    row.dataset.key = key;

    var keyCell = document.createElement('td');
    keyCell.className = 'state-key';
    keyCell.textContent = displayKey;

    var valCell = document.createElement('td');
    var fmt = this._formatValue(value);
    valCell.className = 'state-value ' + fmt.className;
    valCell.textContent = fmt.display;

    row.appendChild(keyCell);
    row.appendChild(valCell);
    group.tbody.appendChild(row);

    this._stateMap.set(key, { keyEl: keyCell, valueEl: valCell, row: row, group: groupKey });
  }

  _filterState(term) {
    this._stateMap.forEach(function (entry, key) {
      var visible = !term || key.toLowerCase().indexOf(term) !== -1;
      entry.row.style.display = visible ? '' : 'none';
    });
  }

  // ------------------------------------------------
  // Rendering — Phases panel (kept for Task 6)
  // ------------------------------------------------

  _renderPhases() {
    var panel = this.panels.phases;
    var hasTimeline = this.phaseTimeline.length > 0;
    var data = hasTimeline ? this.phaseTimeline : this.phases;
    var self = this;

    if (data.length === 0 && !this.telemetry.current_phase) {
      panel.innerHTML = '<div class="events-empty">No phase changes yet</div>';
      return;
    }

    var html = '';

    // Current phase hero card
    var currentPhase = this.telemetry.current_phase || (data.length > 0 ? data[data.length - 1].to : null);
    if (currentPhase) {
      html += '<div class="phase-hero">' +
        '<div class="phase-hero-label">Current Phase</div>' +
        '<div class="phase-hero-name">' + this._esc(currentPhase) + '</div>' +
        '</div>';
    }

    // Phase transition entries with duration bars
    var totalMs = Date.now() - this.sessionStart;

    if (data.length > 0) {
      html += '<div class="phase-entries">';
      data.forEach(function (entry, i) {
        var isTimeline = entry.duration_secs !== undefined;
        var durationMs = isTimeline ? entry.duration_secs * 1000 : 0;
        var pct = totalMs > 0 ? Math.min(100, (durationMs / totalMs) * 100) : 0;
        var durationStr = isTimeline
          ? (entry.duration_secs < 1 ? (entry.duration_secs * 1000).toFixed(0) + 'ms' : entry.duration_secs.toFixed(1) + 's')
          : '';

        var isCurrent = i === data.length - 1;
        var triggerLabel = entry.trigger || entry.reason || '';
        var triggerClass = triggerLabel.includes('programmatic') ? 'programmatic' : 'guard';

        html += '<div class="phase-entry' + (isCurrent ? ' current' : '') + '">' +
          '<div class="phase-entry-header">' +
          '<span class="phase-dot' + (isCurrent ? ' active' : '') + '"></span>' +
          '<span class="phase-from">' + self._esc(entry.from) + '</span>' +
          '<span class="phase-arrow">&rarr;</span>' +
          '<span class="phase-to">' + self._esc(entry.to) + '</span>' +
          (durationStr ? '<span class="phase-dur">' + durationStr + '</span>' : '') +
          '</div>';

        if (pct > 0) {
          html += '<div class="phase-bar-track"><div class="phase-bar-fill" style="width:' + pct + '%"></div></div>';
        }

        if (triggerLabel) {
          html += '<div class="phase-entry-trigger"><span class="phase-trigger ' + triggerClass + '">' + self._esc(triggerLabel) + '</span>';
          if (entry.turn !== undefined) {
            html += '<span class="phase-turn">turn ' + entry.turn + '</span>';
          }
          html += '</div>';
        }

        html += '</div>';
      });
      html += '</div>';
    }

    panel.innerHTML = html;
    panel.scrollTop = panel.scrollHeight;
  }

  // ------------------------------------------------
  // Rendering — Metrics panel (Task 4)
  // ------------------------------------------------

  _renderMetrics() {
    var panel = this.panels.metrics;
    var stats = this.telemetry;

    if (!stats || Object.keys(stats).length === 0) {
      panel.innerHTML = '<div class="events-empty">No metrics yet</div>';
      return;
    }

    var avg = Math.round(stats.avg_response_latency_ms || 0);
    var last = Math.round(stats.last_response_latency_ms || 0);
    var minL = Math.round(stats.min_response_latency_ms || 0);
    var maxL = Math.round(stats.max_response_latency_ms || 0);
    var responses = stats.response_count || 0;

    // Token counts
    var totalTokens = stats.total_token_count || 0;
    var promptTokens = stats.prompt_token_count || 0;
    var responseTokens = stats.response_token_count || 0;

    // Cost estimation (Gemini 2.0 Flash pricing)
    var cost = promptTokens * 0.000000075 + responseTokens * 0.0000003;

    // Uptime formatting
    var uptimeSecs = stats.uptime_secs || 0;
    var uptimeMin = Math.floor(uptimeSecs / 60);
    var uptimeSec = Math.floor(uptimeSecs % 60);
    var uptimeStr = uptimeMin > 0
      ? uptimeMin + 'm ' + (uptimeSec < 10 ? '0' : '') + uptimeSec + 's'
      : uptimeSec + 's';

    var html = '<div class="metrics-content">';

    // Three-column hero layout
    html += '<div class="metrics-heroes">';

    // Latency hero
    html += '<div class="metrics-hero">' +
      '<div class="metrics-hero-label">Latency</div>' +
      '<div class="metrics-hero-value">' + avg + '<span class="nfr-unit">ms</span></div>' +
      '<div class="metrics-hero-sub">' +
      'last ' + last + 'ms';
    if (responses > 1) {
      html += '<br>' + minL + ' &ndash; ' + maxL + 'ms';
    }
    html += '</div></div>';

    // Tokens hero
    html += '<div class="metrics-hero">' +
      '<div class="metrics-hero-label">Tokens</div>' +
      '<div class="metrics-hero-value">' + totalTokens.toLocaleString() + '</div>' +
      '<div class="metrics-hero-sub">' +
      promptTokens.toLocaleString() + ' prompt<br>' +
      responseTokens.toLocaleString() + ' response<br>' +
      'est. ~$' + cost.toFixed(6) +
      '</div></div>';

    // Session hero
    html += '<div class="metrics-hero">' +
      '<div class="metrics-hero-label">Session</div>' +
      '<div class="metrics-hero-value">' + uptimeStr + '</div>' +
      '<div class="metrics-hero-sub">' +
      responses + ' turns<br>' +
      (stats.interruptions || 0) + ' interruptions';
    if (stats.current_phase) {
      html += '<br>phase: ' + this._esc(stats.current_phase);
    }
    html += '</div></div>';

    html += '</div>'; // .metrics-heroes

    // Latency range visualization (kept from original)
    if (responses > 1) {
      var range = maxL - minL;
      var lastPct = range > 0 ? Math.min(100, Math.max(0, (last - minL) / range * 100)) : 50;
      var avgPct = range > 0 ? Math.min(100, Math.max(0, (avg - minL) / range * 100)) : 50;

      html += '<div class="nfr-range-vis" style="margin:0 0 4px; border-radius:6px; border:1px solid var(--border-light);">' +
        '<div class="nfr-range-labels"><span>' + minL + 'ms</span><span>' + maxL + 'ms</span></div>' +
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

    // Per-turn latency sparkline
    if (this.turnLatencies.length > 0) {
      html += '<div class="metrics-sparkline-wrap">' +
        '<div class="metrics-sparkline-label">Per-Turn Latency</div>' +
        '<canvas class="metrics-sparkline" id="metrics-sparkline-canvas"></canvas>' +
        '</div>';
    }

    // Audio section (kept from original)
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
        '</div></div>';
    }

    // Tool calls section (kept from original)
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

    html += '</div>'; // .metrics-content
    panel.innerHTML = html;

    // Render sparkline after innerHTML update
    var sparkCanvas = panel.querySelector('#metrics-sparkline-canvas');
    if (sparkCanvas && this.turnLatencies.length > 0) {
      this._sparkline = new Sparkline(sparkCanvas);
      this._sparkline.setData(this.turnLatencies);
      this._sparkline.render();
    }

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
      case 'spanEvent':
        var durDisplay = msg.duration_us > 1000
          ? (msg.duration_us / 1000).toFixed(1) + 'ms'
          : msg.duration_us + 'us';
        return (msg.name || '') + '  ' + (msg.status || '') + '  ' + durDisplay;
      case 'turnMetrics':
        return 'turn ' + msg.turn + '  ' + msg.latency_ms + 'ms  ' + msg.prompt_tokens + '/' + msg.response_tokens + ' tokens';
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
