/**
 * Single requestAnimationFrame loop with dirty-flag scheduling.
 * Register named renderers; mark them dirty to batch into the next frame.
 *
 * Demand-driven: the rAF loop only runs when something is dirty.
 * markDirty() kicks the loop; loop stops when nothing is dirty.
 */
function RenderScheduler() {
  this._renderers = {};      // name -> renderFn
  this._dirty = {};          // name -> true (acts as Set)
  this._hasDirty = false;
  this._rafId = 0;
  this._stopped = false;
  this._boundTick = this._tick.bind(this);
}

/**
 * Register a named renderer function.
 * @param {string}   name     - Unique renderer name
 * @param {function} renderFn - Called once per dirty frame
 */
RenderScheduler.prototype.register = function (name, renderFn) {
  this._renderers[name] = renderFn;
  if (this._stopped) {
    this._stopped = false;
  }
};

/**
 * Remove a named renderer.
 * @param {string} name
 */
RenderScheduler.prototype.unregister = function (name) {
  delete this._renderers[name];
  delete this._dirty[name];
};

/**
 * Mark a renderer as needing re-render on the next frame.
 * Kicks the rAF loop if it's idle.
 * @param {string} name
 */
RenderScheduler.prototype.markDirty = function (name) {
  if (this._renderers[name]) {
    this._dirty[name] = true;
    this._hasDirty = true;
    this._scheduleLoop();
  }
};

/**
 * Cancel the rAF loop and clear all dirty flags.
 */
RenderScheduler.prototype.stop = function () {
  this._stopped = true;
  this._cancelLoop();
  this._dirty = {};
  this._hasDirty = false;
};

// --- Internal ---

RenderScheduler.prototype._scheduleLoop = function () {
  if (!this._rafId && !this._stopped) {
    this._rafId = requestAnimationFrame(this._boundTick);
  }
};

RenderScheduler.prototype._cancelLoop = function () {
  if (this._rafId) {
    cancelAnimationFrame(this._rafId);
    this._rafId = 0;
  }
};

RenderScheduler.prototype._tick = function () {
  this._rafId = 0;
  if (this._stopped) return;

  // Process dirty renderers
  if (this._hasDirty) {
    var dirty = this._dirty;
    this._dirty = {};
    this._hasDirty = false;
    var renderers = this._renderers;
    for (var name in dirty) {
      if (dirty[name] && renderers[name]) {
        renderers[name]();
      }
    }
  }

  // Only continue if a renderer marked itself dirty during execution
  if (this._hasDirty) {
    this._scheduleLoop();
  }
  // Otherwise loop stops — markDirty() will kick it again
};
