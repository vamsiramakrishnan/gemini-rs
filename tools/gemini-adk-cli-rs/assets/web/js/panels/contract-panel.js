/**
 * contract-panel.js — Runtime contract explorer.
 *
 * Shows the declared SDK/runtime shape: tools, phases, extractors,
 * promotions, computed values, watchers, and control-lane settings.
 */
var ContractPanel = (function () {
  'use strict';

  var U = DevtoolsUtils;

  function ContractPanel() {
    this.container = null;
    this.contract = null;
  }

  ContractPanel.prototype.create = function (container) {
    this.container = container;
    container.className = 'devtools-panel contract-panel';
    this.render();
  };

  ContractPanel.prototype.setContract = function (contract) {
    this.contract = contract || null;
    this.render();
  };

  ContractPanel.prototype.reset = function () {
    this.contract = null;
    this.render();
  };

  ContractPanel.prototype.render = function () {
    if (!this.container) return;
    this.container.innerHTML = '';

    if (!this.contract) {
      var empty = U.el('div', 'contract-empty');
      empty.innerHTML =
        '<div class="contract-empty-title">No runtime contract yet</div>' +
        '<div class="contract-empty-sub">Connect a session to inspect the SDK-declared flow.</div>';
      this.container.appendChild(empty);
      return;
    }

    var c = this.contract;
    var body = U.el('div', 'contract-scroll');
    this.container.appendChild(body);

    var header = U.el('div', 'contract-header');
    header.innerHTML =
      '<div class="contract-title">Runtime Contract</div>' +
      '<div class="contract-model">' + U.esc(c.model || 'unknown model') + '</div>';
    body.appendChild(header);

    body.appendChild(summaryGrid(c));
    body.appendChild(controlSection(c.controls || {}));
    body.appendChild(phasesSection(c.phases || [], c.initial_phase || c.initialPhase));
    body.appendChild(toolsSection(c.tools || []));
    body.appendChild(extractorsSection(c.extractors || []));
    body.appendChild(simpleListSection('Computed', c.computed || [], function (item) {
      return '<div class="contract-row-main">' + U.esc(item.key || '') + '</div>' +
        '<div class="contract-row-sub">depends on ' + joinOrNone(item.dependencies) + '</div>';
    }));
    body.appendChild(simpleListSection('Watchers', c.watchers || [], function (item) {
      return '<div class="contract-row-main">' + U.esc(item.key || '') + '</div>' +
        '<div class="contract-row-sub">' + U.esc(item.predicate || 'predicate') +
        (item.blocking ? ' · blocking' : ' · advisory') + '</div>';
    }));
  };

  function summaryGrid(c) {
    var grid = U.el('div', 'contract-summary');
    addSummary(grid, 'Phases', (c.phases || []).length);
    addSummary(grid, 'Tools', (c.tools || []).length);
    addSummary(grid, 'Extractors', (c.extractors || []).length);
    addSummary(grid, 'Watchers', (c.watchers || []).length);
    addSummary(grid, 'Computed', (c.computed || []).length);
    return grid;
  }

  function addSummary(grid, label, value) {
    var item = U.el('div', 'contract-summary-item');
    item.innerHTML = '<div class="contract-summary-value">' + value + '</div>' +
      '<div class="contract-summary-label">' + U.esc(label) + '</div>';
    grid.appendChild(item);
  }

  function controlSection(controls) {
    var section = sectionShell('Controls');
    var rows = U.el('div', 'contract-kv');
    addKv(rows, 'Steering', controls.steering_mode || controls.steeringMode || 'unknown');
    addKv(rows, 'Context delivery', controls.context_delivery || controls.contextDelivery || 'unknown');
    addKv(rows, 'Soft turn timeout', formatMs(controls.soft_turn_timeout_ms || controls.softTurnTimeoutMs));
    addKv(rows, 'Telemetry', formatMs(controls.telemetry_interval_ms || controls.telemetryIntervalMs));
    addKv(rows, 'Tool advisory', controls.tool_advisory || controls.toolAdvisory ? 'enabled' : 'disabled');
    addKv(rows, 'Repair', controls.repair_enabled || controls.repairEnabled ? 'enabled' : 'disabled');
    section.appendChild(rows);
    return section;
  }

  function phasesSection(phases, initialPhase) {
    var section = sectionShell('Phases');
    if (!phases.length) {
      section.appendChild(emptyNote('No phases declared.'));
      return section;
    }
    phases.forEach(function (phase) {
      var card = U.el('div', 'contract-card');
      var name = phase.name || 'phase';
      var badges = [];
      if (name === initialPhase) badges.push('initial');
      if (phase.terminal) badges.push('terminal');
      if (phase.has_guard || phase.hasGuard) badges.push('guarded');
      if (phase.prompt_on_enter || phase.promptOnEnter) badges.push('prompts');
      card.innerHTML =
        '<div class="contract-card-head">' +
          '<div class="contract-card-title">' + U.esc(name) + '</div>' +
          '<div class="contract-badges">' + badges.map(badge).join('') + '</div>' +
        '</div>' +
        '<div class="contract-row-sub">tools: ' + toolsText(phase.tools_enabled || phase.toolsEnabled) + '</div>' +
        '<div class="contract-row-sub">needs: ' + joinOrNone(phase.needs) + '</div>' +
        '<div class="contract-row-sub">requires: ' + joinOrNone(phase.requires) + '</div>' +
        '<div class="contract-row-sub">presents: ' + joinOrNone(phase.presents) + '</div>';

      if ((phase.preparations || []).length) {
        card.appendChild(chipLine('prepares', phase.preparations.map(function (prep) {
          return prep.name + ' -> ' + (prep.produces || []).join(', ');
        })));
      }
      if ((phase.transitions || []).length) {
        card.appendChild(chipLine('transitions', phase.transitions.map(function (t) {
          return name + ' -> ' + t.target + (t.description ? ' (' + t.description + ')' : '');
        })));
      }
      section.appendChild(card);
    });
    return section;
  }

  function toolsSection(tools) {
    var section = sectionShell('Tools');
    if (!tools.length) {
      section.appendChild(emptyNote('No tools declared.'));
      return section;
    }
    tools.forEach(function (tool) {
      var row = U.el('div', 'contract-row');
      row.innerHTML = '<div class="contract-row-main">' + U.esc(tool.name || '') + '</div>' +
        '<div class="contract-row-sub">' + U.esc(tool.description || '') +
        (tool.behavior ? ' · ' + U.esc(tool.behavior) : '') + '</div>';
      section.appendChild(row);
    });
    return section;
  }

  function extractorsSection(extractors) {
    var section = sectionShell('Extractors');
    if (!extractors.length) {
      section.appendChild(emptyNote('No extractors declared.'));
      return section;
    }
    extractors.forEach(function (extractor) {
      var card = U.el('div', 'contract-card');
      card.innerHTML =
        '<div class="contract-card-head">' +
          '<div class="contract-card-title">' + U.esc(extractor.name || '') + '</div>' +
          '<div class="contract-badges">' + badge(extractor.trigger || 'trigger') + badge('window ' + extractor.window_size) + '</div>' +
        '</div>';
      var promotions = extractor.promotions || [];
      if (promotions.length) {
        card.appendChild(chipLine('promotes', promotions.map(function (p) {
          return p.field + ' -> ' + p.state_key + ' [' + p.merge + (p.has_predicate ? ', predicate' : '') + ']';
        })));
      } else {
        card.appendChild(emptyNote('Legacy auto-flattening promotions.'));
      }
      section.appendChild(card);
    });
    return section;
  }

  function simpleListSection(title, items, renderItem) {
    var section = sectionShell(title);
    if (!items.length) {
      section.appendChild(emptyNote('None declared.'));
      return section;
    }
    items.forEach(function (item) {
      var row = U.el('div', 'contract-row');
      row.innerHTML = renderItem(item);
      section.appendChild(row);
    });
    return section;
  }

  function sectionShell(title) {
    var section = U.el('section', 'contract-section');
    var h = U.el('div', 'contract-section-title');
    h.textContent = title;
    section.appendChild(h);
    return section;
  }

  function addKv(container, key, value) {
    var row = U.el('div', 'contract-kv-row');
    row.innerHTML = '<span>' + U.esc(key) + '</span><strong>' + U.esc(value) + '</strong>';
    container.appendChild(row);
  }

  function chipLine(label, values) {
    var wrap = U.el('div', 'contract-chip-line');
    wrap.innerHTML = '<div class="contract-chip-label">' + U.esc(label) + '</div>' +
      '<div class="contract-chip-wrap">' + values.map(function (value) {
        return '<span class="contract-chip">' + U.esc(value) + '</span>';
      }).join('') + '</div>';
    return wrap;
  }

  function badge(text) {
    return '<span class="contract-badge">' + U.esc(text) + '</span>';
  }

  function emptyNote(text) {
    var note = U.el('div', 'contract-empty-note');
    note.textContent = text;
    return note;
  }

  function joinOrNone(values) {
    return values && values.length ? U.esc(values.join(', ')) : 'none';
  }

  function toolsText(values) {
    return values && values.length ? U.esc(values.join(', ')) : 'all';
  }

  function formatMs(value) {
    if (value === null || value === undefined) return 'off';
    return value + 'ms';
  }

  return ContractPanel;
})();
