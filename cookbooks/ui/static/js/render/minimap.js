/**
 * Canvas-rendered 24px-tall event density minimap.
 * Zero DOM — pure canvas operations, sub-0.5ms target.
 *
 * @param {HTMLCanvasElement} canvas
 * @param {object}           opts
 * @param {function}        [opts.onClick] - Called with (ratio) where ratio is 0-1
 */
function Minimap(canvas, opts) {
  this._canvas = canvas;
  this._ctx = canvas.getContext('2d');
  this._events = null;
  this._sessionDuration = 0;
  this._vpStart = 0;
  this._vpEnd = 1;
  this._onClick = (opts && opts.onClick) || null;

  var self = this;
  canvas.addEventListener('click', function (e) {
    if (!self._onClick) return;
    var rect = canvas.getBoundingClientRect();
    var x = e.clientX - rect.left;
    var ratio = x / rect.width;
    ratio = Math.max(0, Math.min(1, ratio));
    self._onClick(ratio);
  });
}

/** @type {Object<string, string>} */
Minimap.COLORS = {
  audio:              '#7627bb',
  text:               '#1967d2',
  textDelta:          '#1967d2',
  textComplete:       '#1967d2',
  state:              '#b06000',
  stateUpdate:        '#b06000',
  phase:              '#e65100',
  phaseChange:        '#e65100',
  tool:               '#1967d2',
  toolCallEvent:      '#1967d2',
  turn:               '#137333',
  turnComplete:       '#137333',
  interrupted:        '#c5221f',
  error:              '#c5221f',
  span:               '#00695c',
  spanEvent:          '#00695c'
};

Minimap.DEFAULT_COLOR = '#9aa0a6';

/**
 * Set the event data source (RingBuffer).
 * @param {RingBuffer} ringBuffer
 */
Minimap.prototype.setEvents = function (ringBuffer) {
  this._events = ringBuffer;
};

/**
 * Set total session duration in milliseconds.
 * @param {number} ms
 */
Minimap.prototype.setSessionDuration = function (ms) {
  this._sessionDuration = ms;
};

/**
 * Set the visible viewport as 0-1 ratios.
 * @param {number} startRatio
 * @param {number} endRatio
 */
Minimap.prototype.setViewport = function (startRatio, endRatio) {
  this._vpStart = startRatio;
  this._vpEnd = endRatio;
};

/**
 * Repaint the canvas.
 */
Minimap.prototype.render = function () {
  var canvas = this._canvas;
  var ctx = this._ctx;
  var dpr = window.devicePixelRatio || 1;
  var cssW = canvas.clientWidth;
  var cssH = canvas.clientHeight;

  // Handle zero-size canvas (hidden or not yet laid out)
  if (cssW === 0 || cssH === 0) return;

  // Size the backing store for hi-DPI
  var bufW = cssW * dpr;
  var bufH = cssH * dpr;
  if (canvas.width !== bufW || canvas.height !== bufH) {
    canvas.width = bufW;
    canvas.height = bufH;
  }

  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

  // Clear
  ctx.clearRect(0, 0, cssW, cssH);

  // Draw event ticks
  var events = this._events;
  var dur = this._sessionDuration;
  if (events && events.length > 0 && dur > 0) {
    var len = events.length;
    for (var i = 0; i < len; i++) {
      var evt = events.get(i);
      if (!evt) continue;
      var x = (evt.timeMs / dur) * cssW;
      var color = Minimap.COLORS[evt.type] || Minimap.DEFAULT_COLOR;
      ctx.fillStyle = color;
      ctx.fillRect(Math.round(x), 0, 1.5, cssH);
    }
  }

  // Draw viewport overlay
  var vpX = this._vpStart * cssW;
  var vpW = (this._vpEnd - this._vpStart) * cssW;

  ctx.globalAlpha = 0.15;
  ctx.fillStyle = '#4285f4';
  ctx.fillRect(vpX, 0, vpW, cssH);

  ctx.globalAlpha = 0.5;
  ctx.strokeStyle = '#4285f4';
  ctx.lineWidth = 1;
  ctx.strokeRect(vpX, 0, vpW, cssH);

  // Reset globalAlpha
  ctx.globalAlpha = 1.0;
};
