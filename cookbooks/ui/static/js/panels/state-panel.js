/**
 * panels/state-panel.js — Map-based state viewer with diff flash.
 *
 * Owns: search filter, collapsible prefix groups, targeted cell updates.
 * Contract: create(container) / update(key, value) / reset()
 */
var StatePanel = (function () {
  'use strict';

  var U = DevtoolsUtils;

  function StatePanel() {
    this._data = {};
    this._map = new Map();        // key -> { keyEl, valueEl, row, group }
    this._groups = {};            // groupKey -> { header, tbody, container }
    this._searchTerm = '';
    this._groupContainer = null;
    this._container = null;
  }

  StatePanel.prototype.create = function (container) {
    this._container = container;
    container.className = 'devtools-panel';
    this._build();
  };

  StatePanel.prototype.update = function (key, value) {
    var prev = this._data[key];
    this._data[key] = value;

    if (this._map.has(key)) {
      var entry = this._map.get(key);
      var fmt = U.formatValue(value);
      entry.valueEl.textContent = fmt.display;
      entry.valueEl.className = 'state-value ' + fmt.className;

      // Flash animation
      entry.row.classList.remove('state-row-flash');
      void entry.row.offsetWidth;
      entry.row.classList.add('state-row-flash');

      // "was: X" for 2 seconds
      if (prev !== undefined) {
        var prevStr = typeof prev === 'string' ? '"' + prev + '"' : JSON.stringify(prev);
        var wasSpan = U.el('span', 'state-was');
        wasSpan.textContent = ' was: ' + U.truncText(prevStr, 30);
        entry.valueEl.appendChild(wasSpan);
        setTimeout(function () { if (wasSpan.parentNode) wasSpan.remove(); }, 2000);
      }
    } else {
      this._addRow(key, value);
    }

    if (this._searchTerm) this._filter(this._searchTerm);
  };

  StatePanel.prototype.reset = function () {
    this._data = {};
    this._map = new Map();
    this._groups = {};
    this._searchTerm = '';
    this._build();
  };

  // --- Internal ---

  StatePanel.prototype._build = function () {
    var self = this;
    this._container.innerHTML = '';
    this._container.classList.add('state-panel');

    var search = U.el('input', 'state-search');
    search.placeholder = 'Filter keys...';
    search.addEventListener('input', function () {
      self._searchTerm = search.value.toLowerCase();
      self._filter(self._searchTerm);
    });
    this._container.appendChild(search);

    var groupContainer = U.el('div', 'state-groups');
    this._container.appendChild(groupContainer);
    this._groupContainer = groupContainer;
  };

  StatePanel.prototype._createGroup = function (groupKey, prefix) {
    var container = U.el('div', 'state-group collapsed');
    var header = U.el('div', 'state-group-header');
    header.textContent = prefix || 'General';
    header.addEventListener('click', function () { container.classList.toggle('collapsed'); });
    container.appendChild(header);

    var table = U.el('table', 'state-table');
    var tbody = document.createElement('tbody');
    table.appendChild(tbody);
    container.appendChild(table);
    this._groupContainer.appendChild(container);

    this._groups[groupKey] = { header: header, tbody: tbody, container: container };

    // General group starts expanded
    if (!prefix) container.classList.remove('collapsed');
  };

  StatePanel.prototype._addRow = function (key, value) {
    var colonIdx = key.indexOf(':');
    var prefix = (colonIdx > 0 && colonIdx < key.length - 1) ? key.substring(0, colonIdx) : null;
    var displayKey = prefix ? key.substring(colonIdx + 1) : key;
    var groupKey = prefix || '_general';

    if (!this._groups[groupKey]) this._createGroup(groupKey, prefix);
    var group = this._groups[groupKey];

    var row = U.el('tr', 'state-row');
    row.dataset.key = key;
    var keyCell = U.el('td', 'state-key');
    keyCell.textContent = displayKey;
    var valCell = U.el('td');
    var fmt = U.formatValue(value);
    valCell.className = 'state-value ' + fmt.className;
    valCell.textContent = fmt.display;

    row.appendChild(keyCell);
    row.appendChild(valCell);
    group.tbody.appendChild(row);

    this._map.set(key, { keyEl: keyCell, valueEl: valCell, row: row, group: groupKey });
  };

  StatePanel.prototype._filter = function (term) {
    this._map.forEach(function (entry, key) {
      entry.row.style.display = (!term || key.toLowerCase().indexOf(term) !== -1) ? '' : 'none';
    });
  };

  return StatePanel;
})();
