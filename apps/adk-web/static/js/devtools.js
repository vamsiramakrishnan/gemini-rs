/**
 * devtools.js — Thin coordinator that owns tabs, scheduler, status bar,
 * resize handle, minimap, and routes events to panel modules.
 *
 * Panel modules (loaded before this file):
 *   DevtoolsUtils   — shared helpers (panels/utils.js)
 *   TimelinePanel    — event stream + VirtualList (panels/timeline-panel.js)
 *   StatePanel       — key-value state viewer (panels/state-panel.js)
 *   PhasesPanel      — phase hero + transitions (panels/phases-panel.js)
 *   MetricsPanel     — latency/tokens/session heroes (panels/metrics-panel.js)
 *   TracePanel       — trace waterfall / flame chart (panels/trace-panel.js)
 *   EvalPanel        — evaluation results viewer (panels/eval-panel.js)
 *   ArtifactPanel    — artifact browser with versions (panels/artifact-panel.js)
 *   EventInspectorPanel — per-event JSON inspector (panels/event-inspector-panel.js)
 *
 * Exports:
 *   DevtoolsManager — the single object app.js interacts with
 */

var DevtoolsManager = (function () {
  'use strict';

  var U = DevtoolsUtils;

  function DevtoolsManager(container) {
    this.container = container;
    this.tabBar = container.querySelector('.devtools-tabs');
    this.contentArea = container.querySelector('.devtools-content');

    // Shared data
    this.events = new RingBuffer(10000);
    this.sessionStart = Date.now();

    // Tabs
    this.activeTab = 'timeline';
    this.availableTabs = ['timeline', 'state', 'metrics'];
    this.panels = {};
    this.tabButtons = {};

    // Scheduler
    this.scheduler = new RenderScheduler();

    // Panel instances
    this._timeline = new TimelinePanel();
    this._state = new StatePanel();
    this._phases = new PhasesPanel();
    this._metrics = new MetricsPanel();
    this._trace = new TracePanel();
    this._eval = new EvalPanel();
    this._artifacts = new ArtifactPanel();
    this._eventInspector = new EventInspectorPanel();

    // Status bar
    this._statusUptimeEl = null;
    this._statusPhaseEl = null;
    this._statusTurnsEl = null;
    this._statusHealthEl = null;
    this._uptimeInterval = null;
    this._traceId = null;
    this._currentPhase = null;

    this._initPanels();
    this._initTabs();
    this._initStatusBar();
    this._initResize();
    this._initMinimap();
  }

  // ------------------------------------------------
  // Panel initialization
  // ------------------------------------------------

  DevtoolsManager.prototype._initPanels = function () {
    var ce = this.contentArea;

    // Timeline
    var tlEl = U.el('div', '');
    ce.appendChild(tlEl);
    this._timeline.create(tlEl, this.scheduler, this.events);
    this.panels.timeline = tlEl;

    // State
    var stEl = U.el('div', '');
    ce.appendChild(stEl);
    this._state.create(stEl);
    this.panels.state = stEl;

    // Phases
    var phEl = U.el('div', '');
    ce.appendChild(phEl);
    this._phases.create(phEl);
    this.panels.phases = phEl;

    // Metrics
    var meEl = U.el('div', '');
    ce.appendChild(meEl);
    this._metrics.create(meEl, this.scheduler, this.events);
    this.panels.metrics = meEl;

    // Trace
    var trEl = U.el('div', '');
    ce.appendChild(trEl);
    this._trace.create(trEl);
    this.panels.traces = trEl;

    // Eval
    var evEl = U.el('div', '');
    ce.appendChild(evEl);
    this._eval.create(evEl);
    this.panels.eval = evEl;

    // Artifacts
    var arEl = U.el('div', '');
    ce.appendChild(arEl);
    this._artifacts.create(arEl);
    this.panels.artifacts = arEl;

    // Event Inspector
    var eiEl = U.el('div', '');
    ce.appendChild(eiEl);
    this._eventInspector.create(eiEl);
    this.panels.events = eiEl;

    // Register status bar refresh
    var self = this;
    this.scheduler.register('statusBar', function () { self._renderStatusBar(); });
  };

  // ------------------------------------------------
  // Minimap
  // ------------------------------------------------

  DevtoolsManager.prototype._initMinimap = function () {
    var self = this;
    var canvas = document.getElementById('minimap-canvas');
    if (!canvas) return;

    this._minimap = new Minimap(canvas, {
      onClick: function (ratio) {
        // Scroll timeline to the clicked position
        var container = self.panels.timeline.querySelector('.timeline-list-container');
        if (container) {
          container.scrollTop = ratio * self.events.length * 28;
        }
      }
    });
    this._minimap.setEvents(this.events);

    this.scheduler.register('minimap', function () {
      self._minimap.setSessionDuration(Date.now() - self.sessionStart);
      self._minimap.render();
    });

    this._timeline.setMinimap(this._minimap);
  };

  // ------------------------------------------------
  // Tabs
  // ------------------------------------------------

  DevtoolsManager.prototype._initTabs = function () {
    this._renderTabs();
    var self = this;
    var collapseBtn = this.tabBar.querySelector('.devtools-collapse-btn');
    if (collapseBtn) {
      collapseBtn.addEventListener('click', function () { self.toggleCollapse(); });
    }
  };

  DevtoolsManager.prototype._renderTabs = function () {
    var existing = this.tabBar.querySelectorAll('.devtools-tab');
    existing.forEach(function (t) { t.remove(); });
    var spacer = this.tabBar.querySelector('.devtools-tab-spacer');
    var self = this;

    this.availableTabs.forEach(function (tabId) {
      var btn = U.el('button', 'devtools-tab' + (tabId === self.activeTab ? ' active' : ''));
      btn.textContent = _tabLabel(tabId);
      btn.dataset.tab = tabId;
      btn.addEventListener('click', function () { self.switchTab(tabId); });
      self.tabButtons[tabId] = btn;
      self.tabBar.insertBefore(btn, spacer);
    });
  };

  DevtoolsManager.prototype.switchTab = function (tabId) {
    this.activeTab = tabId;
    Object.keys(this.tabButtons).forEach(function (k) {
      this.tabButtons[k].classList.toggle('active', k === tabId);
    }.bind(this));
    Object.keys(this.panels).forEach(function (k) {
      this.panels[k].classList.toggle('active', k === tabId);
    }.bind(this));

    // Panels rendered while display:none have zero clientHeight — force
    // a refresh now that the panel is visible again.
    if (tabId === 'timeline' && this._timeline._vl) {
      this._timeline._vl.refresh();
    }
    if (tabId === 'metrics') {
      this.scheduler.markDirty('metrics');
    }
  };

  DevtoolsManager.prototype.toggleCollapse = function () {
    var isCollapsed = this.container.classList.toggle('collapsed');
    var expandBtn = document.querySelector('.devtools-expand-btn');
    if (expandBtn) expandBtn.classList.toggle('visible', isCollapsed);
  };

  DevtoolsManager.prototype.expand = function () {
    this.container.classList.remove('collapsed');
    var expandBtn = document.querySelector('.devtools-expand-btn');
    if (expandBtn) expandBtn.classList.remove('visible');
  };

  // ------------------------------------------------
  // Status bar
  // ------------------------------------------------

  DevtoolsManager.prototype._initStatusBar = function () {
    this._statusUptimeEl = document.getElementById('status-uptime');
    this._statusPhaseEl = document.getElementById('status-phase');
    this._statusTurnsEl = document.getElementById('status-turns');
    this._statusHealthEl = document.getElementById('status-health');
  };

  DevtoolsManager.prototype._renderStatusBar = function () {
    if (this._statusUptimeEl) {
      this._statusUptimeEl.textContent = U.fmtTime(Date.now() - this.sessionStart);
    }

    var traceEl = document.getElementById('status-trace');
    if (traceEl && this._traceId) {
      traceEl.textContent = this._traceId.substring(0, 8);
      traceEl.title = 'Trace ID: ' + this._traceId;
      traceEl.style.display = '';
    } else if (traceEl) {
      traceEl.style.display = 'none';
    }

    // Health dot from metrics panel
    var healthClass = this._metrics.getHealthClass();
    if (this._statusHealthEl && healthClass) {
      this._statusHealthEl.className = 'status-health-dot ' + healthClass;
    }
  };

  DevtoolsManager.prototype._startStatusTicker = function () {
    var self = this;
    this._uptimeInterval = setInterval(function () {
      self.scheduler.markDirty('statusBar');
    }, 1000);
  };

  DevtoolsManager.prototype._stopStatusTicker = function () {
    if (this._uptimeInterval) {
      clearInterval(this._uptimeInterval);
      this._uptimeInterval = null;
    }
    if (this._statusUptimeEl) this._statusUptimeEl.textContent = '--';
    if (this._statusPhaseEl) this._statusPhaseEl.textContent = '--';
    if (this._statusTurnsEl) this._statusTurnsEl.textContent = '0';
    if (this._statusHealthEl) this._statusHealthEl.className = 'status-health-dot';
  };

  // ------------------------------------------------
  // Resize handle
  // ------------------------------------------------

  DevtoolsManager.prototype._initResize = function () {
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
  };

  // ------------------------------------------------
  // Event routing (API surface for app.js)
  // ------------------------------------------------

  DevtoolsManager.prototype.addEvent = function (msg) {
    this._timeline.addEvent(msg);
    this._eventInspector.addEvent(msg);

    // Track trace ID from session span
    if (msg.type === 'spanEvent' && msg.name === 'rs_genai.session') {
      this._traceId = msg.span_id;
      this._metrics.setTraceId(msg.span_id);
      this.scheduler.markDirty('statusBar');
    }

    // Route span events to trace panel
    if (msg.type === 'spanEvent') {
      this._trace.addSpan(msg);
    }

    // Route artifact events
    if (msg.type === 'artifact' || msg.type === 'artifactUpdate') {
      this._artifacts.addArtifact(msg);
    }
  };

  DevtoolsManager.prototype.handleStateUpdate = function (key, value) {
    this._state.update(key, value);
  };

  DevtoolsManager.prototype.handlePhaseChange = function (data) {
    this._currentPhase = data.to || data.phase || null;
    this._phases.addPhase(data);
    if (this._statusPhaseEl && this._currentPhase) {
      this._statusPhaseEl.textContent = this._currentPhase;
    }
    this.scheduler.markDirty('statusBar');
  };

  DevtoolsManager.prototype.handleEvaluation = function (data) {
    this._eval.addResult(data);
  };

  DevtoolsManager.prototype.handleViolation = function (data) {
    // Violations go into the timeline only
  };

  DevtoolsManager.prototype.handleTelemetry = function (stats) {
    // Inject current phase from PhaseChange events (not in SessionTelemetry snapshot)
    if (this._currentPhase) {
      stats.current_phase = this._currentPhase;
    }

    this._metrics.updateTelemetry(stats);

    // Update status bar fields
    if (stats.current_phase && this._statusPhaseEl) {
      this._statusPhaseEl.textContent = stats.current_phase;
    }
    if (stats.response_count !== undefined && this._statusTurnsEl) {
      this._statusTurnsEl.textContent = stats.response_count;
    }

    // Start uptime ticker on first telemetry
    if (!this._uptimeInterval) this._startStatusTicker();

    this.scheduler.markDirty('statusBar');
  };

  DevtoolsManager.prototype.handlePhaseTimeline = function (entries) {
    this._phases.setTimeline(entries);
  };

  DevtoolsManager.prototype.handleToolCallEvent = function (data) {
    this._metrics.addToolCall(data);
  };

  DevtoolsManager.prototype.handleTurnMetrics = function (data) {
    this._metrics.addTurnLatency(data.latency_ms);
  };

  DevtoolsManager.prototype.handleBufferMetrics = function (metrics) {
    this._metrics.updateBufferMetrics(metrics);
  };

  DevtoolsManager.prototype.handleAppMeta = function (info) {
    this.availableTabs = ['timeline', 'events', 'state'];
    var features = (info.features || []).map(function (f) { return f.toLowerCase(); });
    if (features.includes('state-machine') || info.category === 'advanced' || info.category === 'showcase') {
      this.availableTabs.push('phases');
    }
    this.availableTabs.push('metrics');
    this.availableTabs.push('traces');
    if (features.includes('eval') || features.includes('evaluation')) {
      this.availableTabs.push('eval');
    }
    this.availableTabs.push('artifacts');
    this.tabButtons = {};
    this._renderTabs();
    // Always re-apply active tab to guarantee panel visibility is correct
    this.switchTab(this.availableTabs.includes(this.activeTab) ? this.activeTab : 'timeline');
  };

  // ------------------------------------------------
  // Reset
  // ------------------------------------------------

  DevtoolsManager.prototype.reset = function () {
    this.events = new RingBuffer(10000);
    this.sessionStart = Date.now();
    this._traceId = null;
    this._currentPhase = null;

    // Reset all panels
    this._timeline.reset(this.events);
    this._state.reset();
    this._phases.reset();
    this._phases.setSessionStart(this.sessionStart);
    this._metrics.reset();
    this._metrics.setSessionStart(this.sessionStart);
    this._trace.reset();
    this._eval.reset();
    this._artifacts.reset();
    this._eventInspector.reset();
    this._eventInspector.setSessionStart(this.sessionStart);

    // Re-bind minimap
    if (this._minimap) {
      this._minimap.setEvents(this.events);
      this._minimap.setViewport(0, 1);
      this.scheduler.markDirty('minimap');
    }

    this._stopStatusTicker();
  };

  // ------------------------------------------------
  // Static helper
  // ------------------------------------------------

  function _tabLabel(tabId) {
    switch (tabId) {
      case 'timeline': return 'Timeline';
      case 'state': return 'State';
      case 'phases': return 'Phases';
      case 'metrics': return 'Metrics';
      case 'traces': return 'Traces';
      case 'eval': return 'Eval';
      case 'artifacts': return 'Artifacts';
      case 'events': return 'Events';
      default: return tabId;
    }
  }

  return DevtoolsManager;
})();
