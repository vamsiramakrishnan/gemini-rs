/**
 * sparkline.js — Tiny canvas bar chart for per-turn latency visualization
 *
 * Usage:
 *   var spark = new Sparkline(canvasElement);
 *   spark.setData([120, 245, 180, 380]);
 *   spark.render();
 */

function Sparkline(canvas) {
  this.canvas = canvas;
  this.ctx = canvas.getContext('2d');
  this.data = [];
}

Sparkline.prototype.setData = function (arr) {
  this.data = arr || [];
};

Sparkline.prototype.render = function () {
  var canvas = this.canvas;
  var ctx = this.ctx;
  var data = this.data;
  var dpr = window.devicePixelRatio || 1;

  // Scale canvas for devicePixelRatio
  var cssWidth = canvas.clientWidth;
  var cssHeight = canvas.clientHeight;
  canvas.width = cssWidth * dpr;
  canvas.height = cssHeight * dpr;
  ctx.scale(dpr, dpr);

  // Clear
  ctx.clearRect(0, 0, cssWidth, cssHeight);

  if (data.length === 0) return;

  var max = 0;
  for (var i = 0; i < data.length; i++) {
    if (data[i] > max) max = data[i];
  }
  if (max === 0) max = 1;

  var barWidth = Math.max(2, Math.floor(cssWidth / data.length) - 1);
  var gap = 1;
  var totalBarSpace = barWidth + gap;

  for (var j = 0; j < data.length; j++) {
    var val = data[j];
    var barHeight = Math.max(1, (val / max) * (cssHeight - 2));
    var x = j * totalBarSpace;
    var y = cssHeight - barHeight;

    // Color by health threshold
    if (val < 300) {
      ctx.fillStyle = '#1b873f';
    } else if (val < 600) {
      ctx.fillStyle = '#d4820c';
    } else {
      ctx.fillStyle = '#c5221f';
    }

    ctx.fillRect(x, y, barWidth, barHeight);
  }
};
