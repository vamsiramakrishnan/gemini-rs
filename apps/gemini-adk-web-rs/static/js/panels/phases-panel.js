/**
 * panels/phases-panel.js — End-user conversation flow panel.
 *
 * Turns phase-machine internals into a readable live map: where the assistant is,
 * what must happen next, why it moved, and which tools were allowed or blocked.
 */
var PhasesPanel = (function () {
  'use strict';

  var U = DevtoolsUtils;

  var HIDDEN_STATE_PREFIXES = ['session:', 'turn:', 'temp:', 'bg:'];
  var HIDDEN_STATE_KEYS = {
    phase: true,
    navigation_context: true,
    'session:phase': true,
    'session:phase_needs': true
  };

  function PhasesPanel() {
    this._container = null;
    this._currentPhase = null;
    this._state = {};
    this._transitions = [];
    this._tools = [];
    this._promotions = [];
    this._timeline = [];
    this._sessionStart = Date.now();

    this._titleEl = null;
    this._summaryEl = null;
    this._nextEl = null;
    this._stepsEl = null;
    this._needsEl = null;
    this._signalsEl = null;
    this._provenanceEl = null;
    this._toolsEl = null;
    this._historyEl = null;
  }

  PhasesPanel.prototype.create = function (container) {
    this._container = container;
    container.className = 'devtools-panel flow-panel';
    this._buildSkeleton();
    this._render();
  };

  PhasesPanel.prototype.addPhase = function (data) {
    this._transitions.push(data);
    this._currentPhase = data.to || data.phase || this._currentPhase;
    this._render();
  };

  PhasesPanel.prototype.setTimeline = function (entries) {
    this._timeline = entries || [];
    if (this._timeline.length) {
      var last = this._timeline[this._timeline.length - 1];
      this._currentPhase = last.to || this._currentPhase;
    }
    this._render();
  };

  PhasesPanel.prototype.setCurrentPhase = function (name) {
    this._currentPhase = name || this._currentPhase;
    this._render();
  };

  PhasesPanel.prototype.updateState = function (key, value) {
    this._state[key] = value;
    if ((key === 'session:phase' || key === 'phase') && value) {
      this._currentPhase = String(value);
    }
    this._render();
  };

  PhasesPanel.prototype.addToolCall = function (data) {
    this._tools.push(data);
    if (this._tools.length > 8) this._tools.shift();
    this._renderTools();
  };

  PhasesPanel.prototype.addPromotion = function (data) {
    this._promotions.push(data);
    if (this._promotions.length > 12) this._promotions.shift();
    this._renderProvenance();
  };

  PhasesPanel.prototype.setSessionStart = function (ts) {
    this._sessionStart = ts;
  };

  PhasesPanel.prototype.reset = function () {
    this._currentPhase = null;
    this._state = {};
    this._transitions = [];
    this._tools = [];
    this._promotions = [];
    this._timeline = [];
    this._sessionStart = Date.now();
    if (this._container) {
      this._buildSkeleton();
      this._render();
    }
  };

  PhasesPanel.prototype._buildSkeleton = function () {
    this._container.innerHTML = '';

    var header = U.el('section', 'flow-hero');
    var eyebrow = U.el('div', 'flow-eyebrow');
    eyebrow.textContent = 'Live conversation flow';
    this._titleEl = U.el('h2', 'flow-title');
    this._summaryEl = U.el('p', 'flow-summary');
    this._nextEl = U.el('div', 'flow-next');
    header.appendChild(eyebrow);
    header.appendChild(this._titleEl);
    header.appendChild(this._summaryEl);
    header.appendChild(this._nextEl);
    this._container.appendChild(header);

    this._stepsEl = U.el('div', 'flow-steps');
    this._container.appendChild(this._stepsEl);

    this._container.appendChild(section('To move forward'));
    this._needsEl = U.el('div', 'flow-needs');
    this._container.appendChild(this._needsEl);

    this._container.appendChild(section('Important signals'));
    this._signalsEl = U.el('div', 'flow-signals');
    this._container.appendChild(this._signalsEl);

    this._container.appendChild(section('State provenance'));
    this._provenanceEl = U.el('div', 'flow-provenance');
    this._container.appendChild(this._provenanceEl);

    this._container.appendChild(section('Recent actions'));
    this._toolsEl = U.el('div', 'flow-actions');
    this._container.appendChild(this._toolsEl);

    this._container.appendChild(section('Why the flow moved'));
    this._historyEl = U.el('div', 'flow-history');
    this._container.appendChild(this._historyEl);
  };

  PhasesPanel.prototype._render = function () {
    if (!this._container || !this._titleEl) return;

    var phase = this._phase();
    var copy = phaseCopy(phase);
    this._titleEl.textContent = copy.label;
    this._summaryEl.textContent = phaseSummary(phase, this._history());
    this._nextEl.textContent = this._nextText();

    this._renderSteps();
    this._renderNeeds();
    this._renderSignals();
    this._renderProvenance();
    this._renderTools();
    this._renderHistory();
  };

  PhasesPanel.prototype._renderSteps = function () {
    this._stepsEl.innerHTML = '';
    var order = this._phaseOrder();
    var current = this._phase();
    var currentIdx = order.indexOf(current);

    order.forEach(function (phase, idx) {
      var state = currentIdx === -1 ? 'upcoming' : (idx < currentIdx ? 'done' : (idx === currentIdx ? 'current' : 'upcoming'));
      var item = U.el('div', 'flow-step ' + state);
      var rail = U.el('div', 'flow-step-rail');
      var dot = U.el('span', 'flow-step-dot');
      rail.appendChild(dot);
      var body = U.el('div', 'flow-step-body');
      var label = U.el('div', 'flow-step-label');
      label.textContent = phaseLabel(phase);
      var status = U.el('div', 'flow-step-status');
      status.textContent = state === 'done' ? 'Complete' : (state === 'current' ? 'Now' : 'Next');
      body.appendChild(label);
      body.appendChild(status);
      item.appendChild(rail);
      item.appendChild(body);
      this._stepsEl.appendChild(item);
    }.bind(this));
  };

  PhasesPanel.prototype._renderNeeds = function () {
    this._needsEl.innerHTML = '';
    var needs = this._state['session:phase_needs'];
    if (!Array.isArray(needs) || needs.length === 0) {
      this._needsEl.appendChild(emptyNote('No required fields are outstanding for this step.'));
      return;
    }

    needs.forEach(function (key) {
      var captured = hasMeaningfulValue(this._state[key]);
      var card = U.el('div', 'flow-need ' + (captured ? 'done' : 'missing'));
      var label = U.el('div', 'flow-need-label');
      label.textContent = fieldLabel(key);
      var state = U.el('div', 'flow-need-state');
      state.textContent = captured ? friendlyValue(this._state[key]) : 'Still needed';
      card.appendChild(label);
      card.appendChild(state);
      this._needsEl.appendChild(card);
    }.bind(this));
  };

  PhasesPanel.prototype._renderSignals = function () {
    this._signalsEl.innerHTML = '';
    var count = 0;
    this._signalKeys().forEach(function (key) {
      if (!(key in this._state)) return;
      count++;
      var row = U.el('div', 'flow-signal');
      var label = U.el('span', 'flow-signal-label');
      label.textContent = fieldLabel(key);
      var value = U.el('span', 'flow-signal-value ' + signalClass(this._state[key]));
      value.textContent = friendlyValue(this._state[key]);
      row.appendChild(label);
      row.appendChild(value);
      this._signalsEl.appendChild(row);
    }.bind(this));
    if (!count) this._signalsEl.appendChild(emptyNote('Signals will appear as the conversation is understood.'));
  };

  PhasesPanel.prototype._renderTools = function () {
    if (!this._toolsEl) return;
    this._toolsEl.innerHTML = '';
    if (!this._tools.length) {
      this._toolsEl.appendChild(emptyNote('No tools have run yet.'));
      return;
    }

    this._tools.slice().reverse().forEach(function (tool) {
      var blocked = toolResultHasError(tool.result);
      var row = U.el('div', 'flow-action ' + (blocked ? 'blocked' : 'ok'));
      var top = U.el('div', 'flow-action-top');
      var name = U.el('span', 'flow-action-name');
      name.textContent = titleize(tool.name || 'tool');
      var status = U.el('span', 'flow-action-status');
      status.textContent = blocked ? 'Blocked' : 'Allowed';
      top.appendChild(name);
      top.appendChild(status);
      row.appendChild(top);

      var detail = U.el('div', 'flow-action-detail');
      detail.textContent = blocked ? blockedToolText(tool.result) : toolSummary(tool);
      row.appendChild(detail);
      this._toolsEl.appendChild(row);
    }.bind(this));
  };

  PhasesPanel.prototype._renderProvenance = function () {
    if (!this._provenanceEl) return;
    this._provenanceEl.innerHTML = '';
    if (!this._promotions.length) {
      this._provenanceEl.appendChild(emptyNote('State decisions will appear as extractors promote transcript facts.'));
      return;
    }

    this._promotions.slice().reverse().slice(0, 6).forEach(function (promotion) {
      var row = U.el('div', 'flow-promotion ' + (promotion.accepted ? 'accepted' : 'blocked'));
      var top = U.el('div', 'flow-promotion-top');
      var key = U.el('span', 'flow-promotion-key');
      key.textContent = fieldLabel(promotion.state_key || promotion.field);
      var status = U.el('span', 'flow-promotion-status');
      status.textContent = promotion.accepted ? 'Accepted' : 'Blocked';
      top.appendChild(key);
      top.appendChild(status);
      row.appendChild(top);

      var value = U.el('div', 'flow-promotion-value');
      value.textContent = (promotion.extractor || 'extractor') + '.' + (promotion.field || '?') +
        ' = ' + friendlyValue(promotion.value);
      row.appendChild(value);

      var reason = U.el('div', 'flow-promotion-reason');
      reason.textContent = promotion.reason || (promotion.accepted ? 'Promotion rule accepted the value.' : 'Promotion rule blocked the value.');
      row.appendChild(reason);
      this._provenanceEl.appendChild(row);
    }.bind(this));
  };

  PhasesPanel.prototype._renderHistory = function () {
    this._historyEl.innerHTML = '';
    var data = this._history();
    if (!data.length) {
      this._historyEl.appendChild(emptyNote('The first transition will appear here.'));
      return;
    }

    data.slice().reverse().slice(0, 5).forEach(function (entry, idx) {
      var item = U.el('div', 'flow-history-item ' + (idx === 0 ? 'latest' : ''));
      var text = U.el('div', 'flow-history-text');
      text.textContent = phaseLabel(entry.from) + ' -> ' + phaseLabel(entry.to);
      var reason = U.el('div', 'flow-history-reason');
      reason.textContent = readableReason(entry.reason || entry.trigger || '');
      item.appendChild(text);
      item.appendChild(reason);
      this._historyEl.appendChild(item);
    }.bind(this));
  };

  PhasesPanel.prototype._phase = function () {
    return this._currentPhase || this._state['session:phase'] || 'pending';
  };

  PhasesPanel.prototype._history = function () {
    return this._timeline.length ? this._timeline : this._transitions;
  };

  PhasesPanel.prototype._phaseOrder = function () {
    var current = this._phase();
    var seen = [];
    this._history().forEach(function (entry) {
      [entry.from, entry.to].forEach(function (p) {
        if (p && seen.indexOf(p) === -1) seen.push(p);
      });
    });
    if (current && seen.indexOf(current) === -1) seen.push(current);
    return seen.length ? seen : [current];
  };

  PhasesPanel.prototype._signalKeys = function () {
    var needs = this._state['session:phase_needs'];
    var keys = [];
    if (Array.isArray(needs)) {
      needs.forEach(function (key) {
        if (key && keys.indexOf(key) === -1) keys.push(key);
      });
    }

    Object.keys(this._state).sort().forEach(function (key) {
      if (keys.indexOf(key) !== -1) return;
      if (HIDDEN_STATE_KEYS[key]) return;
      if (key.indexOf('derived:') === 0) {
        keys.push(key);
        return;
      }
      if (HIDDEN_STATE_PREFIXES.some(function (prefix) { return key.indexOf(prefix) === 0; })) return;
      keys.push(key);
    });

    return keys.slice(0, 8);
  };

  PhasesPanel.prototype._nextText = function () {
    var needs = this._state['session:phase_needs'];
    if (Array.isArray(needs) && needs.length) {
      var missing = needs.filter(function (key) {
        return !hasMeaningfulValue(this._state[key]);
      }.bind(this));
      if (missing.length) return 'Waiting for ' + missing.map(fieldLabel).join(', ') + '.';
      return 'All requirements for this step are captured.';
    }
    return 'No explicit requirements are outstanding for this step.';
  };

  function section(text) {
    var title = U.el('div', 'flow-section-title');
    title.textContent = text;
    return title;
  }

  function emptyNote(text) {
    var el = U.el('div', 'flow-empty');
    el.textContent = text;
    return el;
  }

  function phaseLabel(phase) {
    return phaseCopy(phase).label;
  }

  function phaseCopy(phase) {
    if (!phase || phase === 'pending') {
      return {
        label: 'Waiting to start',
        summary: 'The session is connected, but the conversation flow has not reported a step yet.'
      };
    }
    return {
      label: titleize(phase),
      summary: 'The assistant is currently operating inside this step.'
    };
  }

  function phaseSummary(phase, history) {
    if (!phase || phase === 'pending') return phaseCopy(phase).summary;
    var last = history.length ? history[history.length - 1] : null;
    if (last && last.to === phase) {
      return 'Entered after ' + readableReason(last.reason || last.trigger || '').toLowerCase();
    }
    return 'The assistant is currently operating inside this step.';
  }

  function fieldLabel(key) {
    return titleize(key);
  }

  function titleize(value) {
    return String(value || '')
      .replace(/^session:/, '')
      .replace(/^derived:/, '')
      .replace(/_/g, ' ')
      .replace(/\b\w/g, function (c) { return c.toUpperCase(); });
  }

  function hasMeaningfulValue(value) {
    if (value === true) return true;
    if (typeof value === 'string') return value.trim().length > 0;
    if (typeof value === 'number') return value !== 0;
    return false;
  }

  function friendlyValue(value) {
    if (value === true) return 'Captured';
    if (value === false) return 'Not yet';
    if (value === null || value === undefined) return 'Not set';
    if (typeof value === 'number') {
      if (value >= 0 && value <= 1) return Math.round(value * 100) + '%';
      return String(value);
    }
    if (typeof value === 'string') return titleize(value);
    if (Array.isArray(value)) return value.length ? value.map(titleize).join(', ') : 'None';
    return U.truncText(JSON.stringify(value), 80);
  }

  function signalClass(value) {
    if (value === true) return 'good';
    if (value === false || value === null || value === undefined) return 'muted';
    return 'good';
  }

  function parseMaybeJson(value) {
    if (typeof value !== 'string') return value;
    try { return JSON.parse(value); } catch (_) { return value; }
  }

  function toolResultHasError(result) {
    var parsed = parseMaybeJson(result);
    return !!(parsed && typeof parsed === 'object' && parsed.error);
  }

  function blockedToolText(result) {
    var parsed = parseMaybeJson(result);
    var error = parsed && typeof parsed === 'object' ? parsed.error : String(result || '');
    if (/not available in the current conversation phase/i.test(error)) {
      return 'The model asked for this before the conversation reached the right step.';
    }
    return U.truncText(error || 'The tool call was blocked.', 120);
  }

  function toolSummary(tool) {
    var result = parseMaybeJson(tool.result);
    if (result && typeof result === 'object') {
      var keys = Object.keys(result).filter(function (key) { return key !== 'error'; });
      if (keys.length) return 'Returned ' + keys.slice(0, 4).map(titleize).join(', ') + '.';
    }
    return U.truncText(String(tool.args || tool.result || 'Completed.'), 100);
  }

  function readableReason(reason) {
    if (!reason) return 'Transition condition was met.';
    return String(reason).replace(/^guard at /, 'Rule matched at ');
  }

  return PhasesPanel;
})();
