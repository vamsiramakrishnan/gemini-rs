/**
 * devtools.js — Devtools panel: State, Events, Playbook, Evaluator, Telemetry tabs
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

    this._initPanels();
    this._initTabs();
  }

  _initPanels() {
    // State panel
    const statePanel = document.createElement('div');
    statePanel.className = 'devtools-panel active';
    statePanel.id = 'panel-state';
    statePanel.innerHTML = '<div class="state-empty">No state yet</div>';
    this.panels.state = statePanel;

    // Events panel
    const eventsPanel = document.createElement('div');
    eventsPanel.className = 'devtools-panel';
    eventsPanel.id = 'panel-events';
    eventsPanel.innerHTML = '<div class="events-empty">No events yet</div>';
    this.panels.events = eventsPanel;

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

    // Telemetry panel
    const telemetryPanel = document.createElement('div');
    telemetryPanel.className = 'devtools-panel telemetry-panel';
    telemetryPanel.id = 'panel-telemetry';
    telemetryPanel.innerHTML = '<div class="events-empty">No telemetry yet</div>';
    this.panels.telemetry = telemetryPanel;

    // Add all to content area
    this.contentArea.appendChild(statePanel);
    this.contentArea.appendChild(eventsPanel);
    this.contentArea.appendChild(playbookPanel);
    this.contentArea.appendChild(evalPanel);
    this.contentArea.appendChild(telemetryPanel);
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
      case 'telemetry': return 'Telemetry';
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

    // Always show telemetry for advanced and showcase apps
    if (info.category === 'advanced' || info.category === 'showcase') {
      this.availableTabs.push('telemetry');
    }

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
    this.panels.events.innerHTML = '<div class="events-empty">No events yet</div>';
    this.panels.playbook.innerHTML = '<div class="events-empty">No phase changes yet</div>';
    this.panels.evaluator.innerHTML = '<div class="events-empty">No evaluations yet</div>';
    this.panels.telemetry.innerHTML = '<div class="events-empty">No telemetry yet</div>';
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
    // Coalesce rapid telemetry updates into a single rAF frame.
    // This prevents DOM thrashing when telemetry arrives faster than
    // the browser can paint (e.g., periodic 2s updates + turn events).
    if (!this._telemetryRafPending) {
      this._telemetryRafPending = true;
      requestAnimationFrame(() => {
        this._telemetryRafPending = false;
        this._renderTelemetry();
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
    this._renderTelemetry();
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
    const panel = this.panels.events;

    // Remove empty message if present
    const empty = panel.querySelector('.events-empty');
    if (empty) empty.remove();

    panel.classList.add('events-panel');

    const entry = document.createElement('div');
    entry.className = 'event-entry';
    entry.innerHTML = `
      <span class="event-time">${event.time}</span>
      <span class="event-type-badge ${event.type}">${event.type}</span>
      <span class="event-content">${event.content}</span>
    `;

    panel.appendChild(entry);

    // Auto-scroll
    panel.scrollTop = panel.scrollHeight;
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

  _renderTelemetry() {
    const panel = this.panels.telemetry;
    const stats = this.telemetry;

    if (!stats || Object.keys(stats).length === 0) {
      panel.innerHTML = '<div class="events-empty">No telemetry yet</div>';
      return;
    }

    const elapsed = Date.now() - this.sessionStart;
    const elapsedDisplay = this._formatElapsed(elapsed);

    let html = '<div class="telemetry-content">';

    // Session duration
    html += `<div class="telemetry-section">
      <div class="telemetry-section-title">Session</div>
      <div class="telemetry-grid">
        <div class="telemetry-stat">
          <div class="telemetry-stat-value">${elapsedDisplay}</div>
          <div class="telemetry-stat-label">Duration</div>
        </div>`;

    if (stats.turn_count !== undefined) {
      html += `<div class="telemetry-stat">
          <div class="telemetry-stat-value">${stats.turn_count}</div>
          <div class="telemetry-stat-label">Turns</div>
        </div>`;
    }

    if (stats.current_phase) {
      html += `<div class="telemetry-stat">
          <div class="telemetry-stat-value phase-badge">${this._esc(stats.current_phase)}</div>
          <div class="telemetry-stat-label">Phase</div>
        </div>`;
    }

    html += '</div></div>';

    // Performance — response latency & audio throughput
    if (stats.response_count !== undefined || stats.audio_chunks_out !== undefined) {
      html += `<div class="telemetry-section">
        <div class="telemetry-section-title">Performance</div>
        <div class="telemetry-grid">`;

      if (stats.last_response_latency_ms !== undefined && stats.response_count > 0) {
        const latency = stats.last_response_latency_ms;
        const cls = latency < 300 ? 'telemetry-stat-good' : latency < 600 ? 'telemetry-stat-ok' : 'telemetry-stat-warn';
        html += `<div class="telemetry-stat">
            <div class="telemetry-stat-value ${cls}">${latency}<span class="telemetry-stat-unit">ms</span></div>
            <div class="telemetry-stat-label">Last RTT</div>
          </div>`;
      }

      if (stats.avg_response_latency_ms !== undefined && stats.response_count > 0) {
        const avg = stats.avg_response_latency_ms;
        const cls = avg < 300 ? 'telemetry-stat-good' : avg < 600 ? 'telemetry-stat-ok' : 'telemetry-stat-warn';
        html += `<div class="telemetry-stat">
            <div class="telemetry-stat-value ${cls}">${avg}<span class="telemetry-stat-unit">ms</span></div>
            <div class="telemetry-stat-label">Avg RTT</div>
          </div>`;
      }

      if (stats.interruptions !== undefined) {
        html += `<div class="telemetry-stat">
            <div class="telemetry-stat-value">${stats.interruptions}</div>
            <div class="telemetry-stat-label">Barge-ins</div>
          </div>`;
      }

      html += '</div>';

      // Latency range bar (min / max)
      if (stats.response_count > 1 && stats.min_response_latency_ms !== undefined) {
        html += `<div class="telemetry-latency-range">
          <span class="telemetry-range-label">min</span>
          <span class="telemetry-range-value">${stats.min_response_latency_ms}ms</span>
          <span class="telemetry-range-bar"></span>
          <span class="telemetry-range-value">${stats.max_response_latency_ms}ms</span>
          <span class="telemetry-range-label">max</span>
        </div>`;
      }

      // Audio throughput row
      if (stats.audio_chunks_out !== undefined) {
        html += `<div class="telemetry-grid" style="margin-top: 6px;">
          <div class="telemetry-stat">
            <div class="telemetry-stat-value">${stats.audio_kbytes_out || 0}<span class="telemetry-stat-unit">KB</span></div>
            <div class="telemetry-stat-label">Audio Out</div>
          </div>
          <div class="telemetry-stat">
            <div class="telemetry-stat-value">${stats.audio_throughput_kbps || 0}<span class="telemetry-stat-unit">KB/s</span></div>
            <div class="telemetry-stat-label">Throughput</div>
          </div>
          <div class="telemetry-stat">
            <div class="telemetry-stat-value">${stats.uptime_secs || 0}<span class="telemetry-stat-unit">s</span></div>
            <div class="telemetry-stat-label">Uptime</div>
          </div>
        </div>`;
      }

      html += '</div>';
    }

    // State stats
    const stateKeyCount = Object.keys(this.stateData).length;
    if (stateKeyCount > 0 || stats.extractor_runs !== undefined) {
      html += `<div class="telemetry-section">
        <div class="telemetry-section-title">State</div>
        <div class="telemetry-grid">
          <div class="telemetry-stat">
            <div class="telemetry-stat-value">${stateKeyCount}</div>
            <div class="telemetry-stat-label">Keys</div>
          </div>`;

      if (stats.extractor_runs !== undefined) {
        html += `<div class="telemetry-stat">
            <div class="telemetry-stat-value">${stats.extractor_runs}</div>
            <div class="telemetry-stat-label">Extractions</div>
          </div>`;
      }

      if (stats.computed_vars !== undefined) {
        html += `<div class="telemetry-stat">
            <div class="telemetry-stat-value">${stats.computed_vars}</div>
            <div class="telemetry-stat-label">Computed</div>
          </div>`;
      }

      html += '</div></div>';
    }

    // Phase stats
    if (stats.phase_count !== undefined || this.phases.length > 0) {
      html += `<div class="telemetry-section">
        <div class="telemetry-section-title">Phases</div>
        <div class="telemetry-grid">
          <div class="telemetry-stat">
            <div class="telemetry-stat-value">${stats.phase_count || this.phases.length}</div>
            <div class="telemetry-stat-label">Transitions</div>
          </div>`;

      if (stats.current_phase_duration) {
        html += `<div class="telemetry-stat">
            <div class="telemetry-stat-value">${stats.current_phase_duration}</div>
            <div class="telemetry-stat-label">In Phase</div>
          </div>`;
      }

      if (stats.avg_turn_duration_ms !== undefined && stats.avg_turn_duration_ms > 0) {
        const secs = (stats.avg_turn_duration_ms / 1000).toFixed(1);
        html += `<div class="telemetry-stat">
            <div class="telemetry-stat-value">${secs}<span class="telemetry-stat-unit">s</span></div>
            <div class="telemetry-stat-label">Avg Turn</div>
          </div>`;
      }

      html += '</div></div>';
    }

    // Tool calls
    if (this.toolCalls.length > 0) {
      html += `<div class="telemetry-section">
        <div class="telemetry-section-title">Tools</div>
        <div class="telemetry-grid">
          <div class="telemetry-stat">
            <div class="telemetry-stat-value">${this.toolCalls.length}</div>
            <div class="telemetry-stat-label">Calls</div>
          </div>
        </div>
        <div class="telemetry-tool-list">`;

      // Show last 5 tool calls
      const recentTools = this.toolCalls.slice(-5);
      recentTools.forEach(tc => {
        html += `<div class="telemetry-tool-entry">
          <span class="telemetry-tool-name">${this._esc(tc.name)}</span>
          <span class="telemetry-tool-args">${this._truncate(tc.args, 40)}</span>
          <span class="telemetry-tool-result">${this._truncate(tc.result, 40)}</span>
        </div>`;
      });

      html += '</div></div>';
    }

    // Violations summary
    if (this.violations.length > 0) {
      html += `<div class="telemetry-section">
        <div class="telemetry-section-title">Guardrails</div>
        <div class="telemetry-grid">
          <div class="telemetry-stat">
            <div class="telemetry-stat-value telemetry-stat-warn">${this.violations.length}</div>
            <div class="telemetry-stat-label">Violations</div>
          </div>
        </div>
      </div>`;
    }

    html += '</div>';
    panel.innerHTML = html;
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
