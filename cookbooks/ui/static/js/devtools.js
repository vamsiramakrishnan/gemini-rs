/**
 * devtools.js — Devtools panel: State, Events, Playbook, Evaluator, NFR tabs
 *
 * Exports:
 *   DevtoolsManager — manages devtools panel state and rendering
 */

class DevtoolsManager {
  /**
   * @param {HTMLElement} container  The .devtools-pane element
   */
  constructor(container) {
    this.container = container;
    this.tabBar = container.querySelector('.devtools-tabs');
    this.contentArea = container.querySelector('.devtools-content');

    // State
    this.stateData = {};
    this.events = [];
    this.phases = [];
    this.evaluations = [];
    this.violations = [];
    this.telemetry = {};
    this.phaseTimeline = [];
    this.toolCalls = [];
    this._telemetryRafPending = false;

    // Current tab
    this.activeTab = 'state';

    // Available tabs (updated when appMeta arrives)
    this.availableTabs = ['state', 'events'];

    // DOM references
    this.panels = {};
    this.tabButtons = {};

    // Session start time for relative timestamps
    this.sessionStart = Date.now();

    // Event type filters — types to HIDE from the events panel
    this.hiddenEventTypes = new Set(['audio']);

    // Status bar elements
    this._statusUptimeEl = null;
    this._statusPhaseEl = null;
    this._statusTurnsEl = null;
    this._statusRafId = null;

    this._initPanels();
    this._initTabs();
    this._initStatusBar();
    this._initResize();
  }

  _initPanels() {
    // State panel
    const statePanel = document.createElement('div');
    statePanel.className = 'devtools-panel active';
    statePanel.id = 'panel-state';
    statePanel.innerHTML = '<div class="state-empty">No state yet</div>';
    this.panels.state = statePanel;

    // Events panel (with filter bar + scrollable event list)
    const eventsPanel = document.createElement('div');
    eventsPanel.className = 'devtools-panel events-panel-wrapper';
    eventsPanel.id = 'panel-events';

    // Filter bar
    const filterBar = document.createElement('div');
    filterBar.className = 'events-filter-bar';
    filterBar.innerHTML = `
      <span class="events-filter-label">Hide:</span>
      <label class="events-filter-toggle">
        <input type="checkbox" data-filter="audio" checked> audio
      </label>
      <label class="events-filter-toggle">
        <input type="checkbox" data-filter="telemetry"> telemetry
      </label>
      <label class="events-filter-toggle">
        <input type="checkbox" data-filter="voiceActivityStart"> vad
      </label>
    `;
    filterBar.addEventListener('change', (e) => {
      const cb = e.target;
      if (!cb.dataset.filter) return;
      const types = cb.dataset.filter.split(',');
      types.forEach(t => {
        if (cb.checked) this.hiddenEventTypes.add(t);
        else this.hiddenEventTypes.delete(t);
      });
      // Also hide voiceActivityEnd when vad is hidden
      if (cb.dataset.filter === 'voiceActivityStart') {
        if (cb.checked) this.hiddenEventTypes.add('voiceActivityEnd');
        else this.hiddenEventTypes.delete('voiceActivityEnd');
      }
      this._reRenderEvents();
    });
    eventsPanel.appendChild(filterBar);

    // Scrollable event list
    const eventList = document.createElement('div');
    eventList.className = 'events-list';
    eventList.innerHTML = '<div class="events-empty">No events yet</div>';
    eventsPanel.appendChild(eventList);
    this._eventList = eventList;     // inner scrollable list for rendering
    this.panels.events = eventsPanel; // wrapper for tab switching

    // Playbook panel
    const playbookPanel = document.createElement('div');
    playbookPanel.className = 'devtools-panel playbook-panel';
    playbookPanel.id = 'panel-playbook';
    playbookPanel.innerHTML = '<div class="events-empty">No phase changes yet</div>';
    this.panels.playbook = playbookPanel;

    // Evaluator panel
    const evalPanel = document.createElement('div');
    evalPanel.className = 'devtools-panel evaluator-panel';
    evalPanel.id = 'panel-evaluator';
    evalPanel.innerHTML = '<div class="events-empty">No evaluations yet</div>';
    this.panels.evaluator = evalPanel;

    // NFR panel (replaces old Telemetry panel)
    const nfrPanel = document.createElement('div');
    nfrPanel.className = 'devtools-panel nfr-panel';
    nfrPanel.id = 'panel-nfr';
    nfrPanel.innerHTML = '<div class="events-empty">No metrics yet</div>';
    this.panels.nfr = nfrPanel;

    // Add all to content area
    this.contentArea.appendChild(statePanel);
    this.contentArea.appendChild(eventsPanel);
    this.contentArea.appendChild(playbookPanel);
    this.contentArea.appendChild(evalPanel);
    this.contentArea.appendChild(nfrPanel);
  }

  _initTabs() {
    this._renderTabs();

    // Collapse button
    const collapseBtn = this.tabBar.querySelector('.devtools-collapse-btn');
    if (collapseBtn) {
      collapseBtn.addEventListener('click', () => this.toggleCollapse());
    }
  }

  _renderTabs() {
    // Clear existing tab buttons but keep the spacer and collapse btn
    const existing = this.tabBar.querySelectorAll('.devtools-tab');
    existing.forEach(t => t.remove());

    const spacer = this.tabBar.querySelector('.devtools-tab-spacer');

    this.availableTabs.forEach(tabId => {
      const btn = document.createElement('button');
      btn.className = 'devtools-tab' + (tabId === this.activeTab ? ' active' : '');
      btn.textContent = this._tabLabel(tabId);
      btn.dataset.tab = tabId;
      btn.addEventListener('click', () => this.switchTab(tabId));
      this.tabButtons[tabId] = btn;
      this.tabBar.insertBefore(btn, spacer);
    });
  }

  _tabLabel(tabId) {
    switch (tabId) {
      case 'state': return 'State';
      case 'events': return 'Events';
      case 'playbook': return 'Playbook';
      case 'evaluator': return 'Evaluator';
      case 'nfr': return 'NFR';
      default: return tabId;
    }
  }

  /**
   * Switch to a tab.
   * @param {string} tabId
   */
  switchTab(tabId) {
    this.activeTab = tabId;

    // Update tab buttons
    Object.entries(this.tabButtons).forEach(([id, btn]) => {
      btn.classList.toggle('active', id === tabId);
    });

    // Update panels
    Object.entries(this.panels).forEach(([id, panel]) => {
      panel.classList.toggle('active', id === tabId);
    });
  }

  /**
   * Toggle devtools collapsed state.
   */
  toggleCollapse() {
    const isCollapsed = this.container.classList.toggle('collapsed');
    const expandBtn = document.querySelector('.devtools-expand-btn');
    if (expandBtn) {
      expandBtn.classList.toggle('visible', isCollapsed);
    }
  }

  /**
   * Expand devtools if collapsed.
   */
  expand() {
    this.container.classList.remove('collapsed');
    const expandBtn = document.querySelector('.devtools-expand-btn');
    if (expandBtn) {
      expandBtn.classList.remove('visible');
    }
  }

  /**
   * Handle appMeta to configure which tabs are visible.
   * @param {object} info  AppInfo from server
   */
  handleAppMeta(info) {
    this.availableTabs = ['state', 'events'];

    const features = (info.features || []).map(f => f.toLowerCase());

    if (features.includes('state-machine') || info.category === 'advanced') {
      this.availableTabs.push('playbook');
    }

    if (features.includes('evaluation') || features.includes('guardrails') || info.category === 'showcase') {
      this.availableTabs.push('evaluator');
    }

    // Always show NFR tab
    this.availableTabs.push('nfr');

    this._renderTabs();

    // If current tab is not available, switch to state
    if (!this.availableTabs.includes(this.activeTab)) {
      this.switchTab('state');
    }
  }

  /**
   * Reset devtools state for a new session.
   */
  reset() {
    this.stateData = {};
    this.events = [];
    this.phases = [];
    this.evaluations = [];
    this.violations = [];
    this.telemetry = {};
    this.phaseTimeline = [];
    this.toolCalls = [];
    this.sessionStart = Date.now();

    this.panels.state.innerHTML = '<div class="state-empty">No state yet</div>';
    this._eventList.innerHTML = '<div class="events-empty">No events yet</div>';
    this.panels.playbook.innerHTML = '<div class="events-empty">No phase changes yet</div>';
    this.panels.evaluator.innerHTML = '<div class="events-empty">No evaluations yet</div>';
    this.panels.nfr.innerHTML = '<div class="events-empty">No metrics yet</div>';
    this._stopStatusTicker();
  }

  // ------------------------------------------------
  // Event handlers for ServerMessage types
  // ------------------------------------------------

  /**
   * Handle any server message as a devtools event.
   * @param {object} msg  The parsed ServerMessage
   */
  addEvent(msg) {
    const elapsed = Date.now() - this.sessionStart;
    const timeStr = this._formatElapsed(elapsed);
    const content = this._eventContent(msg);

    const event = { type: msg.type, time: timeStr, content, raw: msg };
    this.events.push(event);

    this._renderEvent(event);
  }

  /**
   * Handle stateUpdate message.
   * @param {string} key
   * @param {*} value
   */
  handleStateUpdate(key, value) {
    this.stateData[key] = value;
    this._renderState(key);
  }

  /**
   * Handle phaseChange message.
   * @param {object} data  { from, to, reason }
   */
  handlePhaseChange(data) {
    this.phases.push(data);
    this._renderPhases();
  }

  /**
   * Handle evaluation message.
   * @param {object} data  { phase, score, notes }
   */
  handleEvaluation(data) {
    this.evaluations.push(data);
    this._renderEvaluator();
  }

  /**
   * Handle violation message.
   * @param {object} data  { rule, severity, detail }
   */
  handleViolation(data) {
    this.violations.push(data);
    this._renderEvaluator();
  }

  /**
   * Handle telemetry message with session stats.
   * Uses requestAnimationFrame to avoid competing with audio playback.
   * @param {object} stats
   */
  handleTelemetry(stats) {
    this.telemetry = stats;

    // Update status bar from telemetry
    if (stats.current_phase && this._statusPhaseEl) {
      this._statusPhaseEl.textContent = stats.current_phase;
    }
    if (stats.response_count !== undefined && this._statusTurnsEl) {
      this._statusTurnsEl.textContent = stats.response_count;
    }

    // Start the uptime ticker on first telemetry
    if (!this._statusRafId) {
      this._startStatusTicker();
    }

    // Coalesce rapid telemetry updates into a single rAF frame.
    if (!this._telemetryRafPending) {
      this._telemetryRafPending = true;
      requestAnimationFrame(() => {
        this._telemetryRafPending = false;
        this._renderNfr();
      });
    }
  }

  /**
   * Handle phaseTimeline message with enriched phase history.
   * @param {Array} entries
   */
  handlePhaseTimeline(entries) {
    this.phaseTimeline = entries;
    this._renderPhases();
  }

  /**
   * Handle toolCallEvent message.
   * @param {object} data  { name, args, result }
   */
  handleToolCallEvent(data) {
    this.toolCalls.push(data);
    // Tool calls are also reflected in telemetry
    if (!this.telemetry.tool_calls) {
      this.telemetry.tool_calls = 0;
    }
    this.telemetry.tool_calls = this.toolCalls.length;
    this._renderNfr();
  }

  // ------------------------------------------------
  // Rendering
  // ------------------------------------------------

  _renderState(flashKey) {
    const panel = this.panels.state;
    const keys = Object.keys(this.stateData);

    if (keys.length === 0) {
      panel.innerHTML = '<div class="state-empty">No state yet</div>';
      return;
    }

    // Group keys by prefix
    const groups = {};
    const ungrouped = [];
    keys.forEach(key => {
      const colonIdx = key.indexOf(':');
      if (colonIdx > 0 && colonIdx < key.length - 1) {
        const prefix = key.substring(0, colonIdx);
        if (!groups[prefix]) groups[prefix] = [];
        groups[prefix].push(key);
      } else {
        ungrouped.push(key);
      }
    });

    let html = '';

    // Render ungrouped keys first
    if (ungrouped.length > 0) {
      html += this._renderStateGroup(null, ungrouped, flashKey);
    }

    // Render grouped keys with collapsible sections
    const groupOrder = Object.keys(groups).sort();
    groupOrder.forEach(prefix => {
      html += this._renderStateGroup(prefix, groups[prefix].sort(), flashKey);
    });

    panel.innerHTML = html;
    panel.classList.add('state-panel');
  }

  _renderStateGroup(prefix, keys, flashKey) {
    const groupLabel = prefix ? prefix : 'General';
    const groupClass = prefix ? `state-group-${prefix}` : 'state-group-general';

    let html = `<div class="state-group ${groupClass}">`;
    if (prefix) {
      html += `<div class="state-group-header">${this._esc(groupLabel)}</div>`;
    }
    html += '<table class="state-table"><tbody>';

    keys.forEach(key => {
      const value = this.stateData[key];
      const { display, className } = this._formatValue(value);
      const flash = key === flashKey ? ' state-row-flash' : '';
      const displayKey = prefix ? key.substring(prefix.length + 1) : key;
      html += `<tr class="${flash}"><td class="state-key">${this._esc(displayKey)}</td><td class="state-value ${className}">${display}</td></tr>`;
    });

    html += '</tbody></table></div>';
    return html;
  }

  _renderEvent(event) {
    const list = this._eventList;

    // Remove empty message if present
    const empty = list.querySelector('.events-empty');
    if (empty) empty.remove();

    list.classList.add('events-panel');

    const entry = document.createElement('div');
    entry.className = 'event-entry';
    entry.dataset.eventType = event.type;
    entry.innerHTML = `
      <span class="event-time">${event.time}</span>
      <span class="event-type-badge ${event.type}">${event.type}</span>
      <span class="event-content">${event.content}</span>
    `;

    // Hide if filtered
    if (this.hiddenEventTypes.has(event.type)) {
      entry.style.display = 'none';
    }

    list.appendChild(entry);

    // Auto-scroll (only if not hidden)
    if (!this.hiddenEventTypes.has(event.type)) {
      list.scrollTop = list.scrollHeight;
    }
  }

  _reRenderEvents() {
    const list = this._eventList;
    const entries = list.querySelectorAll('.event-entry');
    entries.forEach(entry => {
      const type = entry.dataset.eventType;
      entry.style.display = this.hiddenEventTypes.has(type) ? 'none' : '';
    });
  }

  _renderPhases() {
    const panel = this.panels.playbook;

    // Use enriched timeline if available, otherwise fall back to basic phases
    const hasTimeline = this.phaseTimeline.length > 0;
    const data = hasTimeline ? this.phaseTimeline : this.phases;

    if (data.length === 0) {
      panel.innerHTML = '<div class="events-empty">No phase changes yet</div>';
      return;
    }

    let html = '';

    if (hasTimeline) {
      // Render enriched timeline with durations and triggers
      html += '<div class="phase-timeline">';
      this.phaseTimeline.forEach((entry, i) => {
        const durationDisplay = entry.duration_secs < 1
          ? `${(entry.duration_secs * 1000).toFixed(0)}ms`
          : `${entry.duration_secs.toFixed(1)}s`;
        const triggerLabel = entry.trigger || 'guard';
        const triggerClass = triggerLabel.includes('programmatic') ? 'programmatic' : 'guard';

        html += `<div class="phase-timeline-entry">
          <div class="phase-timeline-left">
            <div class="phase-timeline-dot ${i === this.phaseTimeline.length - 1 ? 'current' : ''}"></div>
            ${i < this.phaseTimeline.length - 1 ? '<div class="phase-timeline-line"></div>' : ''}
          </div>
          <div class="phase-timeline-content">
            <div class="phase-timeline-header">
              <span class="phase-name">${this._esc(entry.from)}</span>
              <span class="phase-arrow">&rarr;</span>
              <span class="phase-name to">${this._esc(entry.to)}</span>
            </div>
            <div class="phase-timeline-meta">
              <span class="phase-trigger ${triggerClass}">${this._esc(triggerLabel)}</span>
              <span class="phase-duration">${durationDisplay}</span>
              <span class="phase-turn">turn ${entry.turn}</span>
            </div>
          </div>
        </div>`;
      });
      html += '</div>';
    } else {
      // Basic phase cards (original behavior)
      this.phases.forEach(p => {
        html += `<div class="phase-card">
          <div class="phase-header">
            <span class="phase-name">${this._esc(p.from)}</span>
            <span class="phase-arrow">&#8594;</span>
            <span class="phase-name">${this._esc(p.to)}</span>
          </div>
          <div class="phase-reason">${this._esc(p.reason)}</div>
        </div>`;
      });
    }

    panel.innerHTML = html;
    panel.scrollTop = panel.scrollHeight;
  }

  _renderEvaluator() {
    const panel = this.panels.evaluator;
    let html = '';

    // Violations first
    this.violations.forEach(v => {
      const sevClass = v.severity === 'warning' ? 'warning' : '';
      html += `<div class="violation-card">
        <div class="violation-header">
          <span class="violation-rule">${this._esc(v.rule)}</span>
          <span class="violation-severity ${sevClass}">${this._esc(v.severity)}</span>
        </div>
        <div class="violation-detail">${this._esc(v.detail)}</div>
      </div>`;
    });

    // Evaluations
    this.evaluations.forEach(e => {
      const scoreClass = e.score >= 0.8 ? 'high' : e.score >= 0.5 ? 'medium' : 'low';
      const scoreDisplay = (e.score * 100).toFixed(0) + '%';
      html += `<div class="eval-card">
        <div class="eval-header">
          <span class="eval-phase">${this._esc(e.phase)}</span>
          <span class="eval-score ${scoreClass}">${scoreDisplay}</span>
        </div>
        <div class="eval-notes">${this._esc(e.notes)}</div>
      </div>`;
    });

    if (html === '') {
      html = '<div class="events-empty">No evaluations yet</div>';
    }

    panel.innerHTML = html;
    panel.scrollTop = panel.scrollHeight;
  }

  _renderNfr() {
    const panel = this.panels.nfr;
    const stats = this.telemetry;

    if (!stats || Object.keys(stats).length === 0) {
      panel.innerHTML = '<div class="events-empty">No metrics yet</div>';
      return;
    }

    let html = '<div class="nfr-content">';

    // Hero: Average Response Latency (TTFB)
    if (stats.response_count > 0) {
      const avg = Math.round(stats.avg_response_latency_ms || 0);
      const last = Math.round(stats.last_response_latency_ms || 0);
      const health = avg < 300 ? 'good' : avg < 600 ? 'ok' : 'warn';
      const healthLabel = avg < 300 ? 'Healthy' : avg < 600 ? 'Moderate' : 'Degraded';

      html += `<div class="nfr-hero nfr-hero-${health}">
        <div class="nfr-hero-header">
          <span class="nfr-hero-dot"></span>
          <span class="nfr-hero-label">Avg Response Latency</span>
          <span class="nfr-hero-health">${healthLabel}</span>
        </div>
        <div class="nfr-hero-value">${avg}<span class="nfr-hero-unit">ms</span></div>
        <div class="nfr-hero-sub">
          <span>Last <strong>${last}ms</strong></span>
          <span class="nfr-hero-sep">&middot;</span>
          <span>${stats.response_count} responses</span>
        </div>
      </div>`;

      // Range visualization with positioned markers
      if (stats.response_count > 1) {
        const min = Math.round(stats.min_response_latency_ms || 0);
        const max = Math.round(stats.max_response_latency_ms || 0);
        const range = max - min;
        const lastPct = range > 0 ? Math.min(100, Math.max(0, (last - min) / range * 100)) : 50;
        const avgPct = range > 0 ? Math.min(100, Math.max(0, (avg - min) / range * 100)) : 50;

        html += `<div class="nfr-range-vis">
          <div class="nfr-range-labels">
            <span>${min}ms</span>
            <span>${max}ms</span>
          </div>
          <div class="nfr-range-track">
            <div class="nfr-range-fill" style="width:100%"></div>
            <div class="nfr-range-marker nfr-range-marker-avg" style="left:${avgPct}%" title="avg ${avg}ms"></div>
            <div class="nfr-range-marker nfr-range-marker-last" style="left:${lastPct}%" title="last ${last}ms"></div>
          </div>
          <div class="nfr-range-legend">
            <span class="nfr-range-legend-item"><span class="nfr-dot-avg"></span>avg</span>
            <span class="nfr-range-legend-item"><span class="nfr-dot-last"></span>last</span>
          </div>
        </div>`;
      }
    }

    // Turn Performance section
    html += `<div class="nfr-section">
      <div class="nfr-section-header">
        <span class="nfr-section-icon turn"></span>
        <span class="nfr-section-title">Turn Performance</span>
      </div>
      <div class="nfr-metric-strip">`;

    if (stats.avg_turn_duration_ms > 0) {
      const secs = (stats.avg_turn_duration_ms / 1000).toFixed(1);
      html += `<div class="nfr-metric">
          <span class="nfr-metric-value">${secs}<span class="nfr-unit">s</span></span>
          <span class="nfr-metric-label">Avg Turn</span>
        </div>`;
    }

    html += `<div class="nfr-metric">
        <span class="nfr-metric-value">${stats.interruptions || 0}</span>
        <span class="nfr-metric-label">Interrupts</span>
      </div>
    </div></div>`;

    // Audio section
    if (stats.audio_chunks_out > 0) {
      html += `<div class="nfr-section">
        <div class="nfr-section-header">
          <span class="nfr-section-icon audio"></span>
          <span class="nfr-section-title">Audio</span>
        </div>
        <div class="nfr-metric-strip">
          <div class="nfr-metric">
            <span class="nfr-metric-value">${stats.audio_kbytes_out || 0}<span class="nfr-unit">KB</span></span>
            <span class="nfr-metric-label">Total Out</span>
          </div>
          <div class="nfr-metric">
            <span class="nfr-metric-value">${stats.audio_throughput_kbps || 0}<span class="nfr-unit">KB/s</span></span>
            <span class="nfr-metric-label">Throughput</span>
          </div>
          <div class="nfr-metric">
            <span class="nfr-metric-value">${stats.uptime_secs || 0}<span class="nfr-unit">s</span></span>
            <span class="nfr-metric-label">Uptime</span>
          </div>
        </div>
      </div>`;
    }

    // Tool Calls section
    if (this.toolCalls.length > 0) {
      html += `<div class="nfr-section">
        <div class="nfr-section-header">
          <span class="nfr-section-icon tools"></span>
          <span class="nfr-section-title">Tool Calls</span>
          <span class="nfr-section-count">${this.toolCalls.length}</span>
        </div>
        <div class="nfr-tool-list">`;

      this.toolCalls.slice(-5).forEach(tc => {
        html += `<div class="nfr-tool-entry">
          <span class="nfr-tool-name">${this._esc(tc.name)}</span>
          <span class="nfr-tool-args">${this._truncate(tc.args, 60)}</span>
          ${tc.result ? `<span class="nfr-tool-result">${this._truncate(tc.result, 80)}</span>` : ''}
        </div>`;
      });

      html += '</div></div>';
    }

    html += '</div>';
    panel.innerHTML = html;

    // Update health indicator in status bar
    this._updateHealthIndicator(stats);
  }

  // --- Status Bar ---

  _initStatusBar() {
    this._statusUptimeEl = document.getElementById('status-uptime');
    this._statusPhaseEl = document.getElementById('status-phase');
    this._statusTurnsEl = document.getElementById('status-turns');
    this._statusHealthEl = document.getElementById('status-health');
  }

  _initResize() {
    const handle = document.getElementById('devtools-resize-handle');
    if (!handle) return;

    let startX, startWidth;

    const onMouseMove = (e) => {
      const dx = startX - e.clientX;
      const newWidth = Math.min(520, Math.max(280, startWidth + dx));
      this.container.style.width = newWidth + 'px';
      this.container.style.minWidth = newWidth + 'px';
      e.preventDefault();
    };

    const onMouseUp = () => {
      handle.classList.remove('active');
      document.removeEventListener('mousemove', onMouseMove);
      document.removeEventListener('mouseup', onMouseUp);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    };

    handle.addEventListener('mousedown', (e) => {
      startX = e.clientX;
      startWidth = this.container.offsetWidth;
      handle.classList.add('active');
      document.body.style.cursor = 'col-resize';
      document.body.style.userSelect = 'none';
      document.addEventListener('mousemove', onMouseMove);
      document.addEventListener('mouseup', onMouseUp);
      e.preventDefault();
    });
  }

  _updateHealthIndicator(stats) {
    if (!this._statusHealthEl) return;
    const avg = stats.avg_response_latency_ms || 0;
    if (stats.response_count > 0) {
      const cls = avg < 300 ? 'good' : avg < 600 ? 'ok' : 'warn';
      this._statusHealthEl.className = 'status-health-dot ' + cls;
    }
  }

  _startStatusTicker() {
    const tick = () => {
      if (this._statusUptimeEl) {
        const elapsed = Date.now() - this.sessionStart;
        this._statusUptimeEl.textContent = this._formatElapsed(elapsed);
      }
      this._statusRafId = requestAnimationFrame(tick);
    };
    this._statusRafId = requestAnimationFrame(tick);
  }

  _stopStatusTicker() {
    if (this._statusRafId) {
      cancelAnimationFrame(this._statusRafId);
      this._statusRafId = null;
    }
    if (this._statusUptimeEl) this._statusUptimeEl.textContent = '--';
    if (this._statusPhaseEl) this._statusPhaseEl.textContent = '--';
    if (this._statusTurnsEl) this._statusTurnsEl.textContent = '0';
    if (this._statusHealthEl) this._statusHealthEl.className = 'status-health-dot';
  }

  // ------------------------------------------------
  // Helpers
  // ------------------------------------------------

  _formatElapsed(ms) {
    const totalSec = Math.floor(ms / 1000);
    const min = Math.floor(totalSec / 60);
    const sec = totalSec % 60;
    const frac = Math.floor((ms % 1000) / 100);
    if (min > 0) {
      return `${min}:${sec.toString().padStart(2, '0')}.${frac}`;
    }
    return `${sec}.${frac}s`;
  }

  _formatValue(value) {
    if (value === null || value === undefined) {
      return { display: 'null', className: 'null' };
    }
    if (typeof value === 'string') {
      return { display: `"${this._esc(value)}"`, className: 'string' };
    }
    if (typeof value === 'number') {
      return { display: String(value), className: 'number' };
    }
    if (typeof value === 'boolean') {
      return { display: String(value), className: 'boolean' };
    }
    // Object / array
    const json = JSON.stringify(value, null, 1);
    const truncated = json.length > 120 ? json.substring(0, 120) + '...' : json;
    return { display: this._esc(truncated), className: '' };
  }

  _eventContent(msg) {
    switch (msg.type) {
      case 'textDelta':
        return this._truncate(msg.text, 80);
      case 'textComplete':
        return this._truncate(msg.text, 80);
      case 'audio':
        const len = msg.data ? msg.data.length : 0;
        return `<span class="truncated">${len} bytes base64</span>`;
      case 'turnComplete':
        return '';
      case 'connected':
        return 'Session established';
      case 'interrupted':
        return 'Model interrupted';
      case 'error':
        return this._esc(msg.message || '');
      case 'stateUpdate':
        return `${this._esc(msg.key)} = ${this._truncate(JSON.stringify(msg.value), 60)}`;
      case 'phaseChange':
        return `${this._esc(msg.from)} -> ${this._esc(msg.to)}`;
      case 'evaluation':
        return `${this._esc(msg.phase)}: ${(msg.score * 100).toFixed(0)}%`;
      case 'violation':
        return `[${this._esc(msg.severity)}] ${this._esc(msg.rule)}`;
      case 'inputTranscription':
        return this._truncate(msg.text, 60);
      case 'outputTranscription':
        return this._truncate(msg.text, 60);
      case 'voiceActivityStart':
        return 'Speech detected';
      case 'voiceActivityEnd':
        return 'Speech ended';
      case 'appMeta':
        return this._esc(msg.info ? msg.info.name : '');
      case 'telemetry':
        return `turns: ${msg.stats.turn_count || 0}`;
      case 'phaseTimeline':
        return `${(msg.entries || []).length} transitions`;
      case 'toolCallEvent':
        return `${this._esc(msg.name)}(${this._truncate(msg.args, 30)})`;
      default:
        return this._truncate(JSON.stringify(msg), 80);
    }
  }

  _truncate(str, max) {
    const escaped = this._esc(String(str));
    if (escaped.length <= max) return escaped;
    return escaped.substring(0, max) + '<span class="truncated">...</span>';
  }

  _esc(str) {
    const div = document.createElement('div');
    div.textContent = str;
    return div.innerHTML;
  }
}
