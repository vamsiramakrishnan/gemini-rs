/**
 * panels/phases-panel.js — Phase hero card with incremental transition entries.
 *
 * Owns: hero display, transition entries with duration bars.
 * Contract: create(container) / addPhase(data) / setTimeline(entries) / setCurrentPhase(name) / reset()
 *
 * Rendering is incremental — only new entries are appended to the DOM.
 */
var PhasesPanel = (function () {
  'use strict';

  var U = DevtoolsUtils;

  function PhasesPanel() {
    this._container = null;
    this._heroNameEl = null;
    this._entriesEl = null;
    this._renderedCount = 0;
    this._phases = [];
    this._timeline = [];
    this._currentPhase = null;
    this._sessionStart = Date.now();
    this._empty = true;
  }

  PhasesPanel.prototype.create = function (container) {
    this._container = container;
    container.className = 'devtools-panel playbook-panel';
    container.innerHTML = '<div class="events-empty">No phase changes yet</div>';
    this._empty = true;
  };

  PhasesPanel.prototype.addPhase = function (data) {
    this._phases.push(data);
    this._render();
  };

  PhasesPanel.prototype.setTimeline = function (entries) {
    this._timeline = entries;
    this._render();
  };

  PhasesPanel.prototype.setCurrentPhase = function (name) {
    this._currentPhase = name;
    if (this._heroNameEl) this._heroNameEl.textContent = name || '';
  };

  PhasesPanel.prototype.setSessionStart = function (ts) {
    this._sessionStart = ts;
  };

  PhasesPanel.prototype.reset = function () {
    this._phases = [];
    this._timeline = [];
    this._currentPhase = null;
    this._renderedCount = 0;
    this._heroNameEl = null;
    this._entriesEl = null;
    this._empty = true;
    this._sessionStart = Date.now();
    this._container.innerHTML = '<div class="events-empty">No phase changes yet</div>';
  };

  // --- Internal ---

  PhasesPanel.prototype._render = function () {
    var data = this._timeline.length > 0 ? this._timeline : this._phases;

    if (data.length === 0 && !this._currentPhase) {
      if (!this._empty) {
        this._container.innerHTML = '<div class="events-empty">No phase changes yet</div>';
        this._empty = true;
        this._heroNameEl = null;
        this._entriesEl = null;
        this._renderedCount = 0;
      }
      return;
    }

    // Build skeleton on first data
    if (this._empty || !this._heroNameEl) {
      this._container.innerHTML = '';
      this._empty = false;

      var hero = U.el('div', 'phase-hero');
      var lbl = U.el('div', 'phase-hero-label');
      lbl.textContent = 'Current Phase';
      hero.appendChild(lbl);
      var name = U.el('div', 'phase-hero-name');
      hero.appendChild(name);
      this._container.appendChild(hero);
      this._heroNameEl = name;

      var entries = U.el('div', 'phase-entries');
      this._container.appendChild(entries);
      this._entriesEl = entries;
      this._renderedCount = 0;
    }

    // Update hero
    var current = this._currentPhase || (data.length > 0 ? data[data.length - 1].to : null);
    this._heroNameEl.textContent = current || '';

    // Append only new entries
    var totalMs = Date.now() - this._sessionStart;
    for (var i = this._renderedCount; i < data.length; i++) {
      this._entriesEl.appendChild(this._createEntry(data[i], i === data.length - 1, totalMs));
    }

    // Update previous last entry to remove "current" styling
    if (this._renderedCount > 0 && this._renderedCount < data.length) {
      var prev = this._entriesEl.children[this._renderedCount - 1];
      if (prev) {
        prev.classList.remove('current');
        var dot = prev.querySelector('.phase-dot');
        if (dot) dot.classList.remove('active');
      }
    }

    this._renderedCount = data.length;
    this._container.scrollTop = this._container.scrollHeight;
  };

  PhasesPanel.prototype._createEntry = function (entry, isCurrent, totalMs) {
    var hasDuration = entry.duration_secs !== undefined;
    var durationMs = hasDuration ? entry.duration_secs * 1000 : 0;
    var pct = totalMs > 0 ? Math.min(100, (durationMs / totalMs) * 100) : 0;
    var durationStr = hasDuration
      ? (entry.duration_secs < 1 ? (entry.duration_secs * 1000).toFixed(0) + 'ms' : entry.duration_secs.toFixed(1) + 's')
      : '';

    var el = U.el('div', 'phase-entry' + (isCurrent ? ' current' : ''));

    var header = U.el('div', 'phase-entry-header');
    var dot = U.el('span', 'phase-dot' + (isCurrent ? ' active' : '')); header.appendChild(dot);
    var from = U.el('span', 'phase-from'); from.textContent = entry.from; header.appendChild(from);
    var arrow = U.el('span', 'phase-arrow'); arrow.innerHTML = '&rarr;'; header.appendChild(arrow);
    var to = U.el('span', 'phase-to'); to.textContent = entry.to; header.appendChild(to);
    if (durationStr) { var dur = U.el('span', 'phase-dur'); dur.textContent = durationStr; header.appendChild(dur); }
    el.appendChild(header);

    if (pct > 0) {
      var track = U.el('div', 'phase-bar-track');
      var fill = U.el('div', 'phase-bar-fill');
      fill.style.width = pct + '%';
      track.appendChild(fill);
      el.appendChild(track);
    }

    var triggerLabel = entry.trigger || entry.reason || '';
    if (triggerLabel) {
      var trigDiv = U.el('div', 'phase-entry-trigger');
      var trigSpan = U.el('span', 'phase-trigger ' + (triggerLabel.includes('programmatic') ? 'programmatic' : 'guard'));
      trigSpan.textContent = triggerLabel;
      trigDiv.appendChild(trigSpan);
      if (entry.turn !== undefined) {
        var turnSpan = U.el('span', 'phase-turn');
        turnSpan.textContent = 'turn ' + entry.turn;
        trigDiv.appendChild(turnSpan);
      }
      el.appendChild(trigDiv);
    }

    return el;
  };

  return PhasesPanel;
})();
