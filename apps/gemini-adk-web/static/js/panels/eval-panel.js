/**
 * panels/eval-panel.js — Evaluation results viewer.
 *
 * Shows eval set results with pass/fail status, per-criterion scores,
 * and test case detail. Supports filtering by status and criterion.
 *
 * Contract: create(container) / addResult(result) / setEvalSet(evalSet) / reset()
 */
var EvalPanel = (function () {
  'use strict';

  var U = DevtoolsUtils;

  function EvalPanel() {
    this._container = null;
    this._results = [];
    this._evalSet = null;
    this._filter = 'all'; // 'all' | 'pass' | 'fail'
    this._listEl = null;
    this._summaryEl = null;
    this._detailEl = null;
    this._selectedResult = null;
  }

  EvalPanel.prototype.create = function (container) {
    this._container = container;
    container.className = 'devtools-panel eval-panel';
    this._build();
  };

  EvalPanel.prototype._build = function () {
    var c = this._container;
    c.innerHTML = '';

    // Header with filter buttons
    var header = U.el('div', 'eval-header');

    var title = U.el('span', 'eval-title');
    title.textContent = 'Evaluation Results';
    header.appendChild(title);

    var filters = U.el('div', 'eval-filters');
    var self = this;

    ['all', 'pass', 'fail'].forEach(function (f) {
      var btn = U.el('button', 'eval-filter-btn' + (f === self._filter ? ' active' : ''));
      btn.textContent = f.charAt(0).toUpperCase() + f.slice(1);
      btn.addEventListener('click', function () {
        self._filter = f;
        self._renderList();
        // Update button states
        filters.querySelectorAll('.eval-filter-btn').forEach(function (b) {
          b.classList.toggle('active', b.textContent.toLowerCase() === f);
        });
      });
      filters.appendChild(btn);
    });
    header.appendChild(filters);
    c.appendChild(header);

    // Summary bar
    this._summaryEl = U.el('div', 'eval-summary');
    this._summaryEl.textContent = 'No results yet';
    c.appendChild(this._summaryEl);

    // Split: list left, detail right
    var split = U.el('div', 'eval-split');

    this._listEl = U.el('div', 'eval-list');
    split.appendChild(this._listEl);

    this._detailEl = U.el('div', 'eval-detail');
    this._detailEl.innerHTML = '<div class="eval-detail-empty">Select a test case</div>';
    split.appendChild(this._detailEl);

    c.appendChild(split);
  };

  EvalPanel.prototype.setEvalSet = function (evalSet) {
    this._evalSet = evalSet;
    this._renderSummary();
  };

  EvalPanel.prototype.addResult = function (result) {
    this._results.push(result);
    this._renderSummary();
    this._renderList();
  };

  EvalPanel.prototype._renderSummary = function () {
    if (!this._summaryEl) return;

    var total = this._results.length;
    var passed = this._results.filter(function (r) { return r.passed; }).length;
    var failed = total - passed;
    var rate = total > 0 ? ((passed / total) * 100).toFixed(0) : 0;

    this._summaryEl.innerHTML = '';

    var stats = [
      { label: 'Total', value: total, cls: '' },
      { label: 'Passed', value: passed, cls: 'eval-stat-pass' },
      { label: 'Failed', value: failed, cls: 'eval-stat-fail' },
      { label: 'Rate', value: rate + '%', cls: parseFloat(rate) >= 80 ? 'eval-stat-pass' : 'eval-stat-fail' }
    ];

    stats.forEach(function (s) {
      var el = U.el('span', 'eval-stat ' + s.cls);
      el.textContent = s.label + ': ' + s.value;
      this._summaryEl.appendChild(el);
    }.bind(this));
  };

  EvalPanel.prototype._renderList = function () {
    if (!this._listEl) return;
    this._listEl.innerHTML = '';

    var filtered = this._results;
    var self = this;

    if (this._filter === 'pass') {
      filtered = filtered.filter(function (r) { return r.passed; });
    } else if (this._filter === 'fail') {
      filtered = filtered.filter(function (r) { return !r.passed; });
    }

    if (filtered.length === 0) {
      var empty = U.el('div', 'eval-list-empty');
      empty.textContent = this._filter === 'all' ? 'No results' : 'No ' + this._filter + 'ed cases';
      this._listEl.appendChild(empty);
      return;
    }

    filtered.forEach(function (result, idx) {
      var row = U.el('div', 'eval-row' + (result === self._selectedResult ? ' selected' : ''));

      var icon = U.el('span', 'eval-icon ' + (result.passed ? 'eval-pass' : 'eval-fail'));
      icon.textContent = result.passed ? '\u2713' : '\u2717';
      row.appendChild(icon);

      var name = U.el('span', 'eval-case-name');
      name.textContent = result.name || ('Case ' + (idx + 1));
      row.appendChild(name);

      var score = U.el('span', 'eval-case-score');
      if (result.score !== undefined) {
        score.textContent = (result.score * 100).toFixed(0) + '%';
      }
      row.appendChild(score);

      row.addEventListener('click', function () {
        self._selectedResult = result;
        self._renderList();
        self._renderDetail(result);
      });

      self._listEl.appendChild(row);
    });
  };

  EvalPanel.prototype._renderDetail = function (result) {
    var d = this._detailEl;
    d.innerHTML = '';

    // Title
    var title = U.el('div', 'eval-detail-title');
    title.textContent = result.name || 'Test Case';
    d.appendChild(title);

    // Status badge
    var badge = U.el('span', 'eval-badge ' + (result.passed ? 'eval-pass' : 'eval-fail'));
    badge.textContent = result.passed ? 'PASSED' : 'FAILED';
    d.appendChild(badge);

    // Overall score
    if (result.score !== undefined) {
      var scoreEl = U.el('div', 'eval-detail-score');
      scoreEl.textContent = 'Overall Score: ' + (result.score * 100).toFixed(1) + '%';
      d.appendChild(scoreEl);
    }

    // Per-criterion scores
    if (result.criteria && result.criteria.length > 0) {
      var critTitle = U.el('div', 'eval-detail-subtitle');
      critTitle.textContent = 'Criteria';
      d.appendChild(critTitle);

      var table = U.el('table', 'eval-detail-table');
      var thead = U.el('tr', '');
      ['Criterion', 'Score', 'Status'].forEach(function (h) {
        var th = U.el('th', '');
        th.textContent = h;
        thead.appendChild(th);
      });
      table.appendChild(thead);

      result.criteria.forEach(function (c) {
        var tr = U.el('tr', '');

        var tdName = U.el('td', '');
        tdName.textContent = c.name;
        tr.appendChild(tdName);

        var tdScore = U.el('td', '');
        tdScore.textContent = c.score !== undefined ? (c.score * 100).toFixed(0) + '%' : '-';
        tr.appendChild(tdScore);

        var tdStatus = U.el('td', c.passed ? 'eval-pass' : 'eval-fail');
        tdStatus.textContent = c.passed ? 'Pass' : 'Fail';
        tr.appendChild(tdStatus);

        table.appendChild(tr);
      });
      d.appendChild(table);
    }

    // Input/output
    if (result.input) {
      var inTitle = U.el('div', 'eval-detail-subtitle');
      inTitle.textContent = 'Input';
      d.appendChild(inTitle);
      var inPre = U.el('pre', 'eval-detail-json');
      inPre.textContent = typeof result.input === 'string' ? result.input : JSON.stringify(result.input, null, 2);
      d.appendChild(inPre);
    }

    if (result.output) {
      var outTitle = U.el('div', 'eval-detail-subtitle');
      outTitle.textContent = 'Output';
      d.appendChild(outTitle);
      var outPre = U.el('pre', 'eval-detail-json');
      outPre.textContent = typeof result.output === 'string' ? result.output : JSON.stringify(result.output, null, 2);
      d.appendChild(outPre);
    }

    if (result.expected) {
      var expTitle = U.el('div', 'eval-detail-subtitle');
      expTitle.textContent = 'Expected';
      d.appendChild(expTitle);
      var expPre = U.el('pre', 'eval-detail-json');
      expPre.textContent = typeof result.expected === 'string' ? result.expected : JSON.stringify(result.expected, null, 2);
      d.appendChild(expPre);
    }
  };

  EvalPanel.prototype.reset = function () {
    this._results = [];
    this._evalSet = null;
    this._filter = 'all';
    this._selectedResult = null;
    if (this._listEl) this._listEl.innerHTML = '';
    if (this._summaryEl) this._summaryEl.textContent = 'No results yet';
    if (this._detailEl) {
      this._detailEl.innerHTML = '<div class="eval-detail-empty">Select a test case</div>';
    }
  };

  return EvalPanel;
})();
