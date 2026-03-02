/**
 * devtools.js — Devtools panel: State, Events, Playbook, Evaluator tabs
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

    // Add all to content area
    this.contentArea.appendChild(statePanel);
    this.contentArea.appendChild(eventsPanel);
    this.contentArea.appendChild(playbookPanel);
    this.contentArea.appendChild(evalPanel);
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
    this.sessionStart = Date.now();

    this.panels.state.innerHTML = '<div class="state-empty">No state yet</div>';
    this.panels.events.innerHTML = '<div class="events-empty">No events yet</div>';
    this.panels.playbook.innerHTML = '<div class="events-empty">No phase changes yet</div>';
    this.panels.evaluator.innerHTML = '<div class="events-empty">No evaluations yet</div>';
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

    let html = '<table class="state-table"><thead><tr><th>Key</th><th>Value</th></tr></thead><tbody>';

    keys.forEach(key => {
      const value = this.stateData[key];
      const { display, className } = this._formatValue(value);
      const flash = key === flashKey ? ' state-row-flash' : '';
      html += `<tr class="${flash}"><td class="state-key">${this._esc(key)}</td><td class="state-value ${className}">${display}</td></tr>`;
    });

    html += '</tbody></table>';
    panel.innerHTML = html;
    panel.classList.add('state-panel');
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

    if (this.phases.length === 0) {
      panel.innerHTML = '<div class="events-empty">No phase changes yet</div>';
      return;
    }

    let html = '';
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
