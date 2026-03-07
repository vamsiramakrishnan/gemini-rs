/**
 * panels/utils.js — Shared utilities for devtools panels.
 *
 * Pure functions, no state. Provides:
 *   DevtoolsUtils.esc(str)        — HTML-escape for innerHTML contexts
 *   DevtoolsUtils.truncText(s, n) — Plain-text truncation (for textContent)
 *   DevtoolsUtils.fmtTime(ms)     — Format elapsed ms as "T+1.2s" or "T+2:05"
 *   DevtoolsUtils.formatValue(v)  — Format a JSON value for display via textContent
 *   DevtoolsUtils.el(tag, cls)    — Create element shorthand
 */
var DevtoolsUtils = (function () {
  'use strict';

  var _escDiv = document.createElement('div');

  function esc(str) {
    _escDiv.textContent = str;
    return _escDiv.innerHTML;
  }

  function truncText(str, max) {
    if (str === null || str === undefined) return '';
    var s = String(str);
    return s.length <= max ? s : s.substring(0, max) + '...';
  }

  function fmtTime(ms) {
    var totalSec = ms / 1000;
    if (totalSec < 60) return 'T+' + totalSec.toFixed(1) + 's';
    var min = Math.floor(totalSec / 60);
    var sec = (totalSec % 60).toFixed(0).padStart(2, '0');
    return 'T+' + min + ':' + sec;
  }

  /**
   * Format a value for textContent display.
   * Returns { display: string, className: string }.
   * Does NOT HTML-escape — caller uses textContent.
   */
  function formatValue(value) {
    if (value === null || value === undefined) return { display: 'null', className: 'null' };
    if (typeof value === 'string') return { display: '"' + value + '"', className: 'string' };
    if (typeof value === 'number') return { display: String(value), className: 'number' };
    if (typeof value === 'boolean') return { display: String(value), className: 'boolean' };
    var json = JSON.stringify(value, null, 1);
    return { display: json.length > 120 ? json.substring(0, 120) + '...' : json, className: '' };
  }

  /** Create an element with optional className. */
  function el(tag, cls) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    return e;
  }

  return { esc: esc, truncText: truncText, fmtTime: fmtTime, formatValue: formatValue, el: el };
})();
