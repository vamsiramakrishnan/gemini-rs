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
    this._traceId = null;

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

    // Cached scratch element for _esc()
    this._escDiv = document.createElement('div');

    // Phases panel tracking (incremental rendering)
    this._phasesEmpty = true;
    this._phaseHeroEl = null;
    this._phaseEntriesEl = null;
    this._phaseRenderedCount = 0;

    // Metrics panel tracking (skeleton + targeted updates)
    this._metricsEmpty = true;
    this._metricsRefs = null;
    this._metricsToolCount = 0;

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

    // Fixed detail panel below the virtual list (not inline — avoids
    // conflicts with VirtualList's translateY recycling)
    var detailPanel = document.createElement('div');
    detailPanel.className = 'tl-detail-panel';
    detailPanel.style.display = 'none';
    var detailClose = document.createElement('button');
    detailClose.className = 'tl-detail-close';
    detailClose.textContent = '\u00d7';
    detailClose.addEventListener('click', function () {
      detailPanel.style.display = 'none';
      self._expandedIdx = -1;
    });
    detailPanel.appendChild(detailClose);
    var detailContent = document.createElement('pre');
    detailContent.className = 'tl-detail-content';
    detailPanel.appendChild(detailContent);
    timelinePanel.appendChild(detailPanel);
    this._detailEl = detailPanel;
    this._detailContent = detailContent;

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

  _applyFilters(newEventOnly) {
    var hidden = this._hiddenTypes;
    var query = this._searchQuery;
    var len = this.events.length;

    if (hidden.size === 0 && !query) {
      this._filteredIndices = null;
      this._timelineVL.setFilter(null);
      return;
    }

    // Fast path: when a single event was appended and no search query,
    // just check the new event instead of rescanning everything
    if (newEventOnly && !query && this._filteredIndices !== null) {
      var lastIdx = len - 1;
      var evt = this.events.get(lastIdx);
      if (evt && !hidden.has(evt.type)) {
        this._filteredIndices.push(lastIdx);
      }
      this._timelineVL.setFilter(this._filteredIndices);
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
    if (this._expandedIdx === idx) {
      this._detailEl.style.display = 'none';
      this._expandedIdx = -1;
      return;
    }

    var event = this.events.get(idx);
    if (!event) return;

    // Update the fixed detail panel content
    try {
      this._detailContent.textContent = JSON.stringify(event.raw, null, 2);
    } catch (e) {
      this._detailContent.textContent = String(event.raw);
    }

    this._detailEl.style.display = '';
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
      this._detailEl.style.display = 'none';
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

    // Reset phases panel tracking
    this._phasesEmpty = true;
    this._phaseHeroEl = null;
    this._phaseEntriesEl = null;
    this._phaseRenderedCount = 0;
    this.panels.phases.innerHTML = '<div class="events-empty">No phase changes yet</div>';

    // Reset metrics panel tracking
    this._metricsEmpty = true;
    this._metricsRefs = null;
    this._metricsToolCount = 0;
    this.panels.metrics.innerHTML = '<div class="events-empty">No metrics yet</div>';

    this._traceId = null;

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

    // Track trace ID from session span
    if (msg.type === 'spanEvent' && msg.name === 'rs_genai.session') {
      this._traceId = msg.span_id;
      this.scheduler.markDirty('statusBar');
    }

    this._applyFilters(true);
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
    var hasTimeline = this.phaseTimeline.length > 0;
    var data = hasTimeline ? this.phaseTimeline : this.phases;

    if (data.length === 0 && !this.telemetry.current_phase) {
      if (!this._phasesEmpty) {
        this.panels.phases.innerHTML = '<div class="events-empty">No phase changes yet</div>';
        this._phasesEmpty = true;
        this._phaseHeroEl = null;
        this._phaseEntriesEl = null;
        this._phaseRenderedCount = 0;
      }
      return;
    }

    var panel = this.panels.phases;
    var self = this;

    // Lazily create hero + entries container
    if (this._phasesEmpty || !this._phaseHeroEl) {
      panel.innerHTML = '';
      this._phasesEmpty = false;

      var hero = document.createElement('div');
      hero.className = 'phase-hero';
      var heroLabel = document.createElement('div');
      heroLabel.className = 'phase-hero-label';
      heroLabel.textContent = 'Current Phase';
      hero.appendChild(heroLabel);
      var heroName = document.createElement('div');
      heroName.className = 'phase-hero-name';
      hero.appendChild(heroName);
      panel.appendChild(hero);
      this._phaseHeroEl = heroName;

      var entries = document.createElement('div');
      entries.className = 'phase-entries';
      panel.appendChild(entries);
      this._phaseEntriesEl = entries;
      this._phaseRenderedCount = 0;
    }

    // Update hero text
    var currentPhase = this.telemetry.current_phase || (data.length > 0 ? data[data.length - 1].to : null);
    this._phaseHeroEl.textContent = currentPhase || '';

    // Append only new entries (incremental)
    var totalMs = Date.now() - this.sessionStart;
    for (var i = this._phaseRenderedCount; i < data.length; i++) {
      var entry = data[i];
      var el = this._createPhaseEntry(entry, i === data.length - 1, totalMs);
      this._phaseEntriesEl.appendChild(el);
    }

    // Update "current" status on the previous last entry
    if (this._phaseRenderedCount > 0 && this._phaseRenderedCount < data.length) {
      var prevLast = this._phaseEntriesEl.children[this._phaseRenderedCount - 1];
      if (prevLast) {
        prevLast.classList.remove('current');
        var prevDot = prevLast.querySelector('.phase-dot');
        if (prevDot) prevDot.classList.remove('active');
      }
    }

    this._phaseRenderedCount = data.length;
    panel.scrollTop = panel.scrollHeight;
  }

  _createPhaseEntry(entry, isCurrent, totalMs) {
    var isTimeline = entry.duration_secs !== undefined;
    var durationMs = isTimeline ? entry.duration_secs * 1000 : 0;
    var pct = totalMs > 0 ? Math.min(100, (durationMs / totalMs) * 100) : 0;
    var durationStr = isTimeline
      ? (entry.duration_secs < 1 ? (entry.duration_secs * 1000).toFixed(0) + 'ms' : entry.duration_secs.toFixed(1) + 's')
      : '';

    var el = document.createElement('div');
    el.className = 'phase-entry' + (isCurrent ? ' current' : '');

    var header = document.createElement('div');
    header.className = 'phase-entry-header';

    var dot = document.createElement('span');
    dot.className = 'phase-dot' + (isCurrent ? ' active' : '');
    header.appendChild(dot);

    var from = document.createElement('span');
    from.className = 'phase-from';
    from.textContent = entry.from;
    header.appendChild(from);

    var arrow = document.createElement('span');
    arrow.className = 'phase-arrow';
    arrow.innerHTML = '&rarr;';
    header.appendChild(arrow);

    var to = document.createElement('span');
    to.className = 'phase-to';
    to.textContent = entry.to;
    header.appendChild(to);

    if (durationStr) {
      var dur = document.createElement('span');
      dur.className = 'phase-dur';
      dur.textContent = durationStr;
      header.appendChild(dur);
    }

    el.appendChild(header);

    if (pct > 0) {
      var barTrack = document.createElement('div');
      barTrack.className = 'phase-bar-track';
      var barFill = document.createElement('div');
      barFill.className = 'phase-bar-fill';
      barFill.style.width = pct + '%';
      barTrack.appendChild(barFill);
      el.appendChild(barTrack);
    }

    var triggerLabel = entry.trigger || entry.reason || '';
    if (triggerLabel) {
      var triggerDiv = document.createElement('div');
      triggerDiv.className = 'phase-entry-trigger';
      var triggerSpan = document.createElement('span');
      triggerSpan.className = 'phase-trigger ' + (triggerLabel.includes('programmatic') ? 'programmatic' : 'guard');
      triggerSpan.textContent = triggerLabel;
      triggerDiv.appendChild(triggerSpan);
      if (entry.turn !== undefined) {
        var turnSpan = document.createElement('span');
        turnSpan.className = 'phase-turn';
        turnSpan.textContent = 'turn ' + entry.turn;
        triggerDiv.appendChild(turnSpan);
      }
      el.appendChild(triggerDiv);
    }

    return el;
  }

  // ------------------------------------------------
  // Rendering — Metrics panel (Task 4)
  // ------------------------------------------------

  _renderMetrics() {
    var stats = this.telemetry;

    if (!stats || Object.keys(stats).length === 0) {
      if (!this._metricsEmpty) {
        this.panels.metrics.innerHTML = '<div class="events-empty">No metrics yet</div>';
        this._metricsEmpty = true;
        this._metricsRefs = null;
      }
      return;
    }

    // Build DOM skeleton on first render, then do targeted text updates
    if (this._metricsEmpty || !this._metricsRefs) {
      this._buildMetricsSkeleton();
      this._metricsEmpty = false;
    }

    var r = this._metricsRefs;
    var avg = Math.round(stats.avg_response_latency_ms || 0);
    var last = Math.round(stats.last_response_latency_ms || 0);
    var minL = Math.round(stats.min_response_latency_ms || 0);
    var maxL = Math.round(stats.max_response_latency_ms || 0);
    var responses = stats.response_count || 0;
    var totalTokens = stats.total_token_count || 0;
    var promptTokens = stats.prompt_token_count || 0;
    var responseTokens = stats.response_token_count || 0;
    var cost = promptTokens * 0.000000075 + responseTokens * 0.0000003;

    var uptimeSecs = stats.uptime_secs || 0;
    var uptimeMin = Math.floor(uptimeSecs / 60);
    var uptimeSec = Math.floor(uptimeSecs % 60);
    var uptimeStr = uptimeMin > 0
      ? uptimeMin + 'm ' + (uptimeSec < 10 ? '0' : '') + uptimeSec + 's'
      : uptimeSec + 's';

    // Update hero values (text only — no DOM creation)
    r.latencyValue.textContent = avg;
    r.latencySub.textContent = 'last ' + last + 'ms' + (responses > 1 ? '\n' + minL + ' \u2013 ' + maxL + 'ms' : '');
    r.tokensValue.textContent = totalTokens.toLocaleString();
    r.tokensSub.textContent = promptTokens.toLocaleString() + ' prompt\n' +
      responseTokens.toLocaleString() + ' response\nest. ~$' + cost.toFixed(6);
    r.sessionValue.textContent = uptimeStr;
    var sessionSubText = responses + ' turns\n' + (stats.interruptions || 0) + ' interruptions';
    if (stats.current_phase) sessionSubText += '\nphase: ' + stats.current_phase;
    r.sessionSub.textContent = sessionSubText;

    // Range visualization
    if (responses > 1) {
      r.rangeVis.style.display = '';
      r.rangeMin.textContent = minL + 'ms';
      r.rangeMax.textContent = maxL + 'ms';
      var range = maxL - minL;
      var avgPct = range > 0 ? Math.min(100, Math.max(0, (avg - minL) / range * 100)) : 50;
      var lastPct = range > 0 ? Math.min(100, Math.max(0, (last - minL) / range * 100)) : 50;
      r.rangeAvgMarker.style.left = avgPct + '%';
      r.rangeAvgMarker.title = 'avg ' + avg + 'ms';
      r.rangeLastMarker.style.left = lastPct + '%';
      r.rangeLastMarker.title = 'last ' + last + 'ms';
    } else {
      r.rangeVis.style.display = 'none';
    }

    // Sparkline — update data and re-render (canvas itself persists)
    if (this.turnLatencies.length > 0) {
      r.sparklineWrap.style.display = '';
      if (!this._sparkline) {
        this._sparkline = new Sparkline(r.sparklineCanvas);
      }
      this._sparkline.setData(this.turnLatencies);
      this._sparkline.render();
    } else {
      r.sparklineWrap.style.display = 'none';
    }

    // Audio section
    if (stats.audio_chunks_out > 0) {
      r.audioSection.style.display = '';
      r.audioKB.textContent = (stats.audio_kbytes_out || 0);
      r.audioKBPS.textContent = (stats.audio_throughput_kbps || 0);
    } else {
      r.audioSection.style.display = 'none';
    }

    // Tool calls section — rebuild only the list when count changes
    if (this.toolCalls.length > 0) {
      r.toolSection.style.display = '';
      r.toolCount.textContent = this.toolCalls.length;
      if (this.toolCalls.length !== this._metricsToolCount) {
        r.toolList.innerHTML = '';
        var self = this;
        this.toolCalls.slice(-5).forEach(function (tc) {
          var div = document.createElement('div');
          div.className = 'nfr-tool-entry';
          var nameSpan = document.createElement('span');
          nameSpan.className = 'nfr-tool-name';
          nameSpan.textContent = tc.name;
          div.appendChild(nameSpan);
          var argsSpan = document.createElement('span');
          argsSpan.className = 'nfr-tool-args';
          argsSpan.textContent = self._truncText(tc.args, 60);
          div.appendChild(argsSpan);
          if (tc.result) {
            var resultSpan = document.createElement('span');
            resultSpan.className = 'nfr-tool-result';
            resultSpan.textContent = self._truncText(tc.result, 80);
            div.appendChild(resultSpan);
          }
          r.toolList.appendChild(div);
        });
        this._metricsToolCount = this.toolCalls.length;
      }
    } else {
      r.toolSection.style.display = 'none';
    }

    this._updateHealthIndicator(stats);
  }

  _buildMetricsSkeleton() {
    var panel = this.panels.metrics;
    panel.innerHTML = '';
    var self = this;

    var content = document.createElement('div');
    content.className = 'metrics-content';

    // Heroes
    var heroes = document.createElement('div');
    heroes.className = 'metrics-heroes';

    var latencyHero = this._createHero('Latency', 'ms');
    heroes.appendChild(latencyHero.el);

    var tokensHero = this._createHero('Tokens');
    heroes.appendChild(tokensHero.el);

    var sessionHero = this._createHero('Session');
    heroes.appendChild(sessionHero.el);

    content.appendChild(heroes);

    // Range visualization (hidden initially)
    var rangeVis = document.createElement('div');
    rangeVis.className = 'nfr-range-vis';
    rangeVis.style.cssText = 'margin:0 0 4px; border-radius:6px; border:1px solid var(--border-light); display:none';
    var rangeLabels = document.createElement('div');
    rangeLabels.className = 'nfr-range-labels';
    var rangeMin = document.createElement('span');
    var rangeMax = document.createElement('span');
    rangeLabels.appendChild(rangeMin);
    rangeLabels.appendChild(rangeMax);
    rangeVis.appendChild(rangeLabels);
    var rangeTrack = document.createElement('div');
    rangeTrack.className = 'nfr-range-track';
    var rangeFill = document.createElement('div');
    rangeFill.className = 'nfr-range-fill';
    rangeFill.style.width = '100%';
    rangeTrack.appendChild(rangeFill);
    var rangeAvgMarker = document.createElement('div');
    rangeAvgMarker.className = 'nfr-range-marker nfr-range-marker-avg';
    rangeTrack.appendChild(rangeAvgMarker);
    var rangeLastMarker = document.createElement('div');
    rangeLastMarker.className = 'nfr-range-marker nfr-range-marker-last';
    rangeTrack.appendChild(rangeLastMarker);
    rangeVis.appendChild(rangeTrack);
    var rangeLegend = document.createElement('div');
    rangeLegend.className = 'nfr-range-legend';
    rangeLegend.innerHTML = '<span class="nfr-range-legend-item"><span class="nfr-dot-avg"></span>avg</span>' +
      '<span class="nfr-range-legend-item"><span class="nfr-dot-last"></span>last</span>';
    rangeVis.appendChild(rangeLegend);
    content.appendChild(rangeVis);

    // Sparkline (hidden initially)
    var sparklineWrap = document.createElement('div');
    sparklineWrap.className = 'metrics-sparkline-wrap';
    sparklineWrap.style.display = 'none';
    var sparklineLabel = document.createElement('div');
    sparklineLabel.className = 'metrics-sparkline-label';
    sparklineLabel.textContent = 'Per-Turn Latency';
    sparklineWrap.appendChild(sparklineLabel);
    var sparklineCanvas = document.createElement('canvas');
    sparklineCanvas.className = 'metrics-sparkline';
    sparklineWrap.appendChild(sparklineCanvas);
    content.appendChild(sparklineWrap);

    // Audio section (hidden initially)
    var audioSection = document.createElement('div');
    audioSection.className = 'nfr-section';
    audioSection.style.display = 'none';
    audioSection.innerHTML = '<div class="nfr-section-header"><span class="nfr-section-icon audio"></span><span class="nfr-section-title">Audio</span></div>';
    var audioStrip = document.createElement('div');
    audioStrip.className = 'nfr-metric-strip';
    var audioKBMetric = document.createElement('div');
    audioKBMetric.className = 'nfr-metric';
    var audioKBVal = document.createElement('span');
    audioKBVal.className = 'nfr-metric-value';
    audioKBMetric.appendChild(audioKBVal);
    var audioKBLabel = document.createElement('span');
    audioKBLabel.className = 'nfr-metric-label';
    audioKBLabel.textContent = 'Total Out (KB)';
    audioKBMetric.appendChild(audioKBLabel);
    audioStrip.appendChild(audioKBMetric);
    var audioKBPSMetric = document.createElement('div');
    audioKBPSMetric.className = 'nfr-metric';
    var audioKBPSVal = document.createElement('span');
    audioKBPSVal.className = 'nfr-metric-value';
    audioKBPSMetric.appendChild(audioKBPSVal);
    var audioKBPSLabel = document.createElement('span');
    audioKBPSLabel.className = 'nfr-metric-label';
    audioKBPSLabel.textContent = 'Throughput (KB/s)';
    audioKBPSMetric.appendChild(audioKBPSLabel);
    audioStrip.appendChild(audioKBPSMetric);
    audioSection.appendChild(audioStrip);
    content.appendChild(audioSection);

    // Tool calls section (hidden initially)
    var toolSection = document.createElement('div');
    toolSection.className = 'nfr-section';
    toolSection.style.display = 'none';
    var toolHeader = document.createElement('div');
    toolHeader.className = 'nfr-section-header';
    toolHeader.innerHTML = '<span class="nfr-section-icon tools"></span><span class="nfr-section-title">Tool Calls</span>';
    var toolCount = document.createElement('span');
    toolCount.className = 'nfr-section-count';
    toolHeader.appendChild(toolCount);
    toolSection.appendChild(toolHeader);
    var toolList = document.createElement('div');
    toolList.className = 'nfr-tool-list';
    toolSection.appendChild(toolList);
    content.appendChild(toolSection);

    // Export button
    var exportDiv = document.createElement('div');
    exportDiv.className = 'metrics-export';
    var exportBtn = document.createElement('button');
    exportBtn.className = 'export-btn';
    exportBtn.textContent = 'Copy Trace as OTLP JSON';
    exportBtn.addEventListener('click', function () { self._exportOtlpJson(); });
    exportDiv.appendChild(exportBtn);
    content.appendChild(exportDiv);

    panel.appendChild(content);

    // Cache all references for targeted updates
    this._metricsRefs = {
      latencyValue: latencyHero.value,
      latencySub: latencyHero.sub,
      tokensValue: tokensHero.value,
      tokensSub: tokensHero.sub,
      sessionValue: sessionHero.value,
      sessionSub: sessionHero.sub,
      rangeVis: rangeVis,
      rangeMin: rangeMin,
      rangeMax: rangeMax,
      rangeAvgMarker: rangeAvgMarker,
      rangeLastMarker: rangeLastMarker,
      sparklineWrap: sparklineWrap,
      sparklineCanvas: sparklineCanvas,
      audioSection: audioSection,
      audioKB: audioKBVal,
      audioKBPS: audioKBPSVal,
      toolSection: toolSection,
      toolCount: toolCount,
      toolList: toolList,
      exportBtn: exportBtn
    };
    this._metricsToolCount = 0;
    this._sparkline = null;
  }

  _createHero(label, unit) {
    var el = document.createElement('div');
    el.className = 'metrics-hero';
    var lbl = document.createElement('div');
    lbl.className = 'metrics-hero-label';
    lbl.textContent = label;
    el.appendChild(lbl);
    var val = document.createElement('div');
    val.className = 'metrics-hero-value';
    // Separate text node for the number so setting it doesn't destroy unit span
    var valText = document.createElement('span');
    val.appendChild(valText);
    if (unit) {
      var unitSpan = document.createElement('span');
      unitSpan.className = 'nfr-unit';
      unitSpan.textContent = unit;
      val.appendChild(unitSpan);
    }
    el.appendChild(val);
    var sub = document.createElement('div');
    sub.className = 'metrics-hero-sub';
    sub.style.whiteSpace = 'pre-line';
    el.appendChild(sub);
    return { el: el, value: valText, sub: sub };
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

    // Show trace ID badge if available
    var traceEl = document.getElementById('status-trace');
    if (traceEl && this._traceId) {
      traceEl.textContent = this._traceId.substring(0, 8);
      traceEl.title = 'Trace ID: ' + this._traceId;
      traceEl.style.display = '';
    } else if (traceEl) {
      traceEl.style.display = 'none';
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

  _formatValue(value) {
    if (value === null || value === undefined) {
      return { display: 'null', className: 'null' };
    }
    if (typeof value === 'string') {
      return { display: '"' + value + '"', className: 'string' };
    }
    if (typeof value === 'number') {
      return { display: String(value), className: 'number' };
    }
    if (typeof value === 'boolean') {
      return { display: String(value), className: 'boolean' };
    }
    var json = JSON.stringify(value, null, 1);
    var truncated = json.length > 120 ? json.substring(0, 120) + '...' : json;
    return { display: truncated, className: '' };
  }

  _esc(str) {
    this._escDiv.textContent = str;
    return this._escDiv.innerHTML;
  }

  _exportOtlpJson() {
    var self = this;
    // Collect all span events from the ring buffer
    var spans = [];
    this.events.forEach(function (e) {
      if (e.type === 'spanEvent') spans.push(e);
    });

    // Format as OTLP-compatible JSON
    var traceId = this._traceId || this._generateTraceId();
    var otlp = {
      resourceSpans: [{
        resource: {
          attributes: [
            { key: 'service.name', value: { stringValue: 'gemini-live-cookbooks' } },
            { key: 'session.start', value: { stringValue: new Date(self.sessionStart).toISOString() } }
          ]
        },
        scopeSpans: [{
          scope: { name: 'rs-genai-ui', version: '0.1.0' },
          spans: spans.map(function (s) {
            return {
              traceId: traceId,
              spanId: s.raw.span_id || '',
              parentSpanId: s.raw.parent_id || '',
              name: s.raw.name || '',
              kind: 1, // SPAN_KIND_INTERNAL
              startTimeUnixNano: String((self.sessionStart + s.timeMs) * 1000000),
              endTimeUnixNano: String((self.sessionStart + s.timeMs + ((s.raw.duration_us || 0) / 1000)) * 1000000),
              attributes: Object.keys(s.raw.attributes || {}).map(function (k) {
                return { key: k, value: { stringValue: String(s.raw.attributes[k]) } };
              }),
              status: { code: s.raw.status === 'ok' ? 1 : 2 }
            };
          })
        }]
      }]
    };

    var json = JSON.stringify(otlp, null, 2);
    var exportBtn = this._metricsRefs && this._metricsRefs.exportBtn;
    navigator.clipboard.writeText(json).then(function () {
      if (exportBtn) {
        exportBtn.textContent = 'Copied!';
        setTimeout(function () { exportBtn.textContent = 'Copy Trace as OTLP JSON'; }, 1500);
      }
    });
  }

  _generateTraceId() {
    var arr = new Uint8Array(16);
    crypto.getRandomValues(arr);
    this._traceId = Array.from(arr).map(function (b) {
      return b.toString(16).padStart(2, '0');
    }).join('');
    return this._traceId;
  }
}
