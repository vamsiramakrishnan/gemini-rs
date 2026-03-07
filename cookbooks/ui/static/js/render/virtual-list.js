/**
 * DOM-recycling virtual scroll list.
 * Positions rows via translateY — zero layout thrashing.
 *
 * @param {HTMLElement} container - Scrollable container element
 * @param {object}      opts
 * @param {number}     [opts.rowHeight=28]  - Pixel height per row
 * @param {number}     [opts.poolSize=80]   - Number of pre-created DOM nodes
 * @param {function}    opts.render          - render(el, item, index)
 */
function VirtualList(container, opts) {
  var self = this;
  this._container = container;
  this._rowHeight = opts.rowHeight || 28;
  this._poolSize = opts.poolSize || 80;
  this._render = opts.render;

  // Data source (RingBuffer or Array)
  this._items = null;
  this._totalCount = 0;

  // Optional filter: array of visible logical indices, or null
  this._filter = null;
  this._visibleCount = 0;

  // Auto-scroll tracking
  this._wasAtBottom = true;

  // rAF debounce
  this._rafId = 0;
  this._destroyed = false;

  // Sentinel to size the scrollable area
  this._sentinel = document.createElement('div');
  this._sentinel.style.cssText = 'position:relative;width:100%;pointer-events:none;';
  container.appendChild(this._sentinel);

  // Pool of reusable DOM nodes
  this._pool = new Array(this._poolSize);
  for (var i = 0; i < this._poolSize; i++) {
    var el = document.createElement('div');
    el.style.cssText =
      'position:absolute;left:0;right:0;overflow:hidden;' +
      'height:' + this._rowHeight + 'px;' +
      'contain:layout style paint;' +
      'will-change:transform;';
    el.style.display = 'none';
    container.appendChild(el);
    this._pool[i] = el;
  }

  // Scroll handler — passive + rAF debounce
  this._onScroll = function () {
    if (self._rafId || self._destroyed) return;
    self._rafId = requestAnimationFrame(function () {
      self._rafId = 0;
      if (!self._destroyed) self._renderVisible();
    });
  };
  container.addEventListener('scroll', this._onScroll, { passive: true });
}

/**
 * Set the data source. Accepts a RingBuffer (has .get()) or plain Array.
 * @param {RingBuffer|Array} items
 */
VirtualList.prototype.setItems = function (items) {
  this._items = items;
  this._totalCount = items.length;
  this._updateVisibleCount();
  this._syncSentinel();
  this._autoScroll();
  this._scheduleRender();
};

/**
 * Set a filter: array of logical indices to display, or null for all.
 * @param {number[]|null} indices
 */
VirtualList.prototype.setFilter = function (indices) {
  this._filter = indices;
  this._updateVisibleCount();
  this._syncSentinel();
  this._scheduleRender();
};

/**
 * Force a re-render of visible rows.
 */
VirtualList.prototype.refresh = function () {
  this._totalCount = this._items ? this._items.length : 0;
  this._updateVisibleCount();
  this._syncSentinel();
  this._autoScroll();
  this._scheduleRender();
};

/**
 * Scroll to the bottom of the list.
 */
VirtualList.prototype.scrollToBottom = function () {
  this._container.scrollTop = this._container.scrollHeight;
  this._wasAtBottom = true;
  this._scheduleRender();
};

/**
 * Clean up event listeners and rAF.
 */
VirtualList.prototype.destroy = function () {
  this._destroyed = true;
  this._container.removeEventListener('scroll', this._onScroll);
  if (this._rafId) {
    cancelAnimationFrame(this._rafId);
    this._rafId = 0;
  }
};

// --- Internal ---

VirtualList.prototype._updateVisibleCount = function () {
  this._visibleCount = this._filter ? this._filter.length : this._totalCount;
};

VirtualList.prototype._syncSentinel = function () {
  this._sentinel.style.height = (this._visibleCount * this._rowHeight) + 'px';
};

VirtualList.prototype._scheduleRender = function () {
  if (this._rafId || this._destroyed) return;
  var self = this;
  this._rafId = requestAnimationFrame(function () {
    self._rafId = 0;
    if (!self._destroyed) self._renderVisible();
  });
};

VirtualList.prototype._autoScroll = function () {
  if (this._wasAtBottom) {
    this._container.scrollTop = this._container.scrollHeight;
  }
};

VirtualList.prototype._getItem = function (logicalIndex) {
  if (!this._items) return undefined;
  // RingBuffer has .get(), Array uses bracket access
  if (typeof this._items.get === 'function') {
    return this._items.get(logicalIndex);
  }
  return this._items[logicalIndex];
};

VirtualList.prototype._renderVisible = function () {
  var ct = this._container;
  var scrollTop = ct.scrollTop;
  var viewHeight = ct.clientHeight;
  var rh = this._rowHeight;
  var count = this._visibleCount;

  // Detect auto-scroll: within 2 rows of end
  var maxScroll = count * rh - viewHeight;
  this._wasAtBottom = scrollTop >= maxScroll - rh * 2;

  // Visible range in virtual space
  var startIdx = Math.max(0, Math.floor(scrollTop / rh));
  var endIdx = Math.min(count, Math.ceil((scrollTop + viewHeight) / rh));

  var poolLen = this._pool.length;

  for (var p = 0; p < poolLen; p++) {
    var row = startIdx + p;
    var el = this._pool[p];
    if (row >= endIdx) {
      el.style.display = 'none';
      continue;
    }
    // Map virtual row to logical data index
    var dataIdx = this._filter ? this._filter[row] : row;
    var item = this._getItem(dataIdx);
    if (item === undefined) {
      el.style.display = 'none';
      continue;
    }
    el.style.display = '';
    el.style.transform = 'translateY(' + (row * rh) + 'px)';
    this._render(el, item, dataIdx);
  }
};
