/**
 * panels/metrics-panel.js — Session metrics with skeleton DOM + targeted updates.
 *
 * Owns: latency/tokens/session heroes, sparkline, range visualization,
 *       audio section, tool calls section, OTel OTLP JSON export.
 * Contract: create(container, scheduler, eventsRef) / updateTelemetry(stats)
 *           / addToolCall(data) / addTurnLatency(ms) / getHealthClass()
 *           / setSessionStart(ts) / setTraceId(id) / reset()
 */
var MetricsPanel = (function () {
  'use strict';

  var U = DevtoolsUtils;

  function MetricsPanel() {
    this._container = null;
    this._scheduler = null;
    this._eventsRef = null;
    this._refs = null;
    this._empty = true;
    this._sparkline = null;
    this._telemetry = {};
    this._turnLatencies = [];
    this._toolCalls = [];
    this._toolRenderedCount = 0;
    this._lastResponseCount = 0;
    this._traceId = null;
    this._sessionStart = Date.now();
    this._bufferMetrics = null;
  }

  MetricsPanel.prototype.create = function (container, scheduler, eventsRef) {
    this._container = container;
    this._scheduler = scheduler;
    this._eventsRef = eventsRef;
    container.className = 'devtools-panel nfr-panel';
    container.innerHTML = '<div class="events-empty">No metrics yet</div>';
    this._empty = true;

    var self = this;
    scheduler.register('metrics', function () { self._render(); });
  };

  MetricsPanel.prototype.updateTelemetry = function (stats) {
    this._telemetry = stats;

    var rc = stats.response_count || 0;
    if (rc > this._lastResponseCount && stats.last_response_latency_ms > 0) {
      this._turnLatencies.push(stats.last_response_latency_ms);
      this._lastResponseCount = rc;
    }

    this._scheduler.markDirty('metrics');
  };

  MetricsPanel.prototype.addToolCall = function (data) {
    this._toolCalls.push(data);
    this._scheduler.markDirty('metrics');
  };

  MetricsPanel.prototype.addTurnLatency = function (ms) {
    this._turnLatencies.push(ms);
    this._scheduler.markDirty('metrics');
  };

  MetricsPanel.prototype.updateBufferMetrics = function (metrics) {
    this._bufferMetrics = metrics;
    this._scheduler.markDirty('metrics');
  };

  MetricsPanel.prototype.setSessionStart = function (ts) {
    this._sessionStart = ts;
  };

  MetricsPanel.prototype.setTraceId = function (id) {
    this._traceId = id;
  };

  MetricsPanel.prototype.getHealthClass = function () {
    var avg = this._telemetry.avg_response_latency_ms || 0;
    var rc = this._telemetry.response_count || 0;
    if (rc === 0) return '';
    return avg < 300 ? 'good' : avg < 600 ? 'ok' : 'warn';
  };

  MetricsPanel.prototype.getTelemetry = function () {
    return this._telemetry;
  };

  MetricsPanel.prototype.getToolCallCount = function () {
    return this._toolCalls.length;
  };

  MetricsPanel.prototype.reset = function () {
    this._telemetry = {};
    this._turnLatencies = [];
    this._toolCalls = [];
    this._toolRenderedCount = 0;
    this._lastResponseCount = 0;
    this._sparkline = null;
    this._refs = null;
    this._empty = true;
    this._traceId = null;
    this._sessionStart = Date.now();
    this._bufferMetrics = null;
    this._container.innerHTML = '<div class="events-empty">No metrics yet</div>';
  };

  // --- Internal ---

  MetricsPanel.prototype._render = function () {
    var stats = this._telemetry;
    if (!stats || Object.keys(stats).length === 0) {
      if (!this._empty) {
        this._container.innerHTML = '<div class="events-empty">No metrics yet</div>';
        this._empty = true;
        this._refs = null;
      }
      return;
    }

    if (this._empty || !this._refs) {
      this._buildSkeleton();
      this._empty = false;
    }

    var r = this._refs;
    var avg = Math.round(stats.avg_response_latency_ms || 0);
    var last = Math.round(stats.last_response_latency_ms || 0);
    var minL = Math.round(stats.min_response_latency_ms || 0);
    var maxL = Math.round(stats.max_response_latency_ms || 0);
    var responses = stats.response_count || 0;
    var totalTokens = stats.total_token_count || 0;
    var promptTokens = stats.prompt_token_count || 0;
    var responseTokens = stats.response_token_count || 0;
    var cost = promptTokens * 0.000000075 + responseTokens * 0.0000003;

    var uptimeSecs = stats.uptime_secs || 0;
    var uptimeMin = Math.floor(uptimeSecs / 60);
    var uptimeSec = Math.floor(uptimeSecs % 60);
    var uptimeStr = uptimeMin > 0
      ? uptimeMin + 'm ' + (uptimeSec < 10 ? '0' : '') + uptimeSec + 's'
      : uptimeSec + 's';

    // Hero values
    r.latencyValue.textContent = avg;
    r.latencySub.textContent = 'last ' + last + 'ms' + (responses > 1 ? '\n' + minL + ' \u2013 ' + maxL + 'ms' : '');
    r.tokensValue.textContent = totalTokens.toLocaleString();
    r.tokensSub.textContent = promptTokens.toLocaleString() + ' prompt\n' +
      responseTokens.toLocaleString() + ' response\nest. ~$' + cost.toFixed(6);
    r.sessionValue.textContent = uptimeStr;
    var sessionSubText = responses + ' turns\n' + (stats.interruptions || 0) + ' interruptions';
    if (stats.current_phase) sessionSubText += '\nphase: ' + stats.current_phase;
    r.sessionSub.textContent = sessionSubText;

    // Range visualization
    if (responses > 1) {
      r.rangeVis.style.display = '';
      r.rangeMin.textContent = minL + 'ms';
      r.rangeMax.textContent = maxL + 'ms';
      var range = maxL - minL;
      var avgPct = range > 0 ? Math.min(100, Math.max(0, (avg - minL) / range * 100)) : 50;
      var lastPct = range > 0 ? Math.min(100, Math.max(0, (last - minL) / range * 100)) : 50;
      r.rangeAvgMarker.style.left = avgPct + '%';
      r.rangeAvgMarker.title = 'avg ' + avg + 'ms';
      r.rangeLastMarker.style.left = lastPct + '%';
      r.rangeLastMarker.title = 'last ' + last + 'ms';
    } else {
      r.rangeVis.style.display = 'none';
    }

    // Sparkline
    if (this._turnLatencies.length > 0) {
      r.sparklineWrap.style.display = '';
      if (!this._sparkline) {
        this._sparkline = new Sparkline(r.sparklineCanvas);
      }
      this._sparkline.setData(this._turnLatencies);
      this._sparkline.render();
    } else {
      r.sparklineWrap.style.display = 'none';
    }

    // Audio section
    if (stats.audio_chunks_out > 0 || this._bufferMetrics) {
      r.audioSection.style.display = '';
      r.audioKB.textContent = (stats.audio_kbytes_out || 0);
      r.audioKBPS.textContent = (stats.audio_throughput_kbps || 0);

      // Jitter buffer metrics
      if (this._bufferMetrics && r.bufferDepth) {
        var bm = this._bufferMetrics;
        r.bufferStrip.style.display = '';
        r.bufferDepth.textContent = bm.depthMs + 'ms';
        r.bufferState.textContent = bm.state;
        r.bufferState.className = 'nfr-metric-value nfr-buf-' + bm.state;
        r.bufferJitter.textContent = bm.jitterMs + 'ms';
        r.bufferUnderruns.textContent = bm.underruns;
      }
    } else {
      r.audioSection.style.display = 'none';
    }

    // Tool calls — rebuild list only when count changes
    if (this._toolCalls.length > 0) {
      r.toolSection.style.display = '';
      r.toolCount.textContent = this._toolCalls.length;
      if (this._toolCalls.length !== this._toolRenderedCount) {
        r.toolList.innerHTML = '';
        this._toolCalls.slice(-5).forEach(function (tc) {
          var div = U.el('div', 'nfr-tool-entry');
          var nameSpan = U.el('span', 'nfr-tool-name');
          nameSpan.textContent = tc.name;
          div.appendChild(nameSpan);
          var argsSpan = U.el('span', 'nfr-tool-args');
          argsSpan.textContent = U.truncText(tc.args, 60);
          div.appendChild(argsSpan);
          if (tc.result) {
            var resultSpan = U.el('span', 'nfr-tool-result');
            resultSpan.textContent = U.truncText(tc.result, 80);
            div.appendChild(resultSpan);
          }
          r.toolList.appendChild(div);
        });
        this._toolRenderedCount = this._toolCalls.length;
      }
    } else {
      r.toolSection.style.display = 'none';
    }
  };

  MetricsPanel.prototype._buildSkeleton = function () {
    var self = this;
    this._container.innerHTML =
      '<div class="metrics-content">' +
        '<div class="metrics-heroes">' +
          '<div class="metrics-hero"><div class="metrics-hero-label">Latency</div>' +
            '<div class="metrics-hero-value"><span data-ref="latencyValue"></span><span class="nfr-unit">ms</span></div>' +
            '<div class="metrics-hero-sub" style="white-space:pre-line" data-ref="latencySub"></div></div>' +
          '<div class="metrics-hero"><div class="metrics-hero-label">Tokens</div>' +
            '<div class="metrics-hero-value"><span data-ref="tokensValue"></span></div>' +
            '<div class="metrics-hero-sub" style="white-space:pre-line" data-ref="tokensSub"></div></div>' +
          '<div class="metrics-hero"><div class="metrics-hero-label">Session</div>' +
            '<div class="metrics-hero-value"><span data-ref="sessionValue"></span></div>' +
            '<div class="metrics-hero-sub" style="white-space:pre-line" data-ref="sessionSub"></div></div>' +
        '</div>' +
        '<div class="nfr-range-vis" style="margin:0 0 4px;border-radius:6px;border:1px solid var(--border-light);display:none" data-ref="rangeVis">' +
          '<div class="nfr-range-labels"><span data-ref="rangeMin"></span><span data-ref="rangeMax"></span></div>' +
          '<div class="nfr-range-track">' +
            '<div class="nfr-range-fill" style="width:100%"></div>' +
            '<div class="nfr-range-marker nfr-range-marker-avg" data-ref="rangeAvgMarker"></div>' +
            '<div class="nfr-range-marker nfr-range-marker-last" data-ref="rangeLastMarker"></div>' +
          '</div>' +
          '<div class="nfr-range-legend">' +
            '<span class="nfr-range-legend-item"><span class="nfr-dot-avg"></span>avg</span>' +
            '<span class="nfr-range-legend-item"><span class="nfr-dot-last"></span>last</span>' +
          '</div>' +
        '</div>' +
        '<div class="metrics-sparkline-wrap" style="display:none" data-ref="sparklineWrap">' +
          '<div class="metrics-sparkline-label">Per-Turn Latency</div>' +
          '<canvas class="metrics-sparkline" data-ref="sparklineCanvas"></canvas>' +
        '</div>' +
        '<div class="nfr-section" style="display:none" data-ref="audioSection">' +
          '<div class="nfr-section-header"><span class="nfr-section-icon audio"></span><span class="nfr-section-title">Audio</span></div>' +
          '<div class="nfr-metric-strip">' +
            '<div class="nfr-metric"><span class="nfr-metric-value" data-ref="audioKB"></span><span class="nfr-metric-label">Total Out (KB)</span></div>' +
            '<div class="nfr-metric"><span class="nfr-metric-value" data-ref="audioKBPS"></span><span class="nfr-metric-label">Throughput (KB/s)</span></div>' +
          '</div>' +
          '<div class="nfr-metric-strip" style="display:none" data-ref="bufferStrip">' +
            '<div class="nfr-metric"><span class="nfr-metric-value" data-ref="bufferDepth"></span><span class="nfr-metric-label">Buffer Depth</span></div>' +
            '<div class="nfr-metric"><span class="nfr-metric-value" data-ref="bufferState"></span><span class="nfr-metric-label">Buffer State</span></div>' +
            '<div class="nfr-metric"><span class="nfr-metric-value" data-ref="bufferJitter"></span><span class="nfr-metric-label">Jitter</span></div>' +
            '<div class="nfr-metric"><span class="nfr-metric-value" data-ref="bufferUnderruns"></span><span class="nfr-metric-label">Underruns</span></div>' +
          '</div>' +
        '</div>' +
        '<div class="nfr-section" style="display:none" data-ref="toolSection">' +
          '<div class="nfr-section-header"><span class="nfr-section-icon tools"></span><span class="nfr-section-title">Tool Calls</span>' +
            '<span class="nfr-section-count" data-ref="toolCount"></span></div>' +
          '<div class="nfr-tool-list" data-ref="toolList"></div>' +
        '</div>' +
        '<div class="metrics-export"><button class="export-btn" data-ref="exportBtn">Copy Trace as OTLP JSON</button></div>' +
      '</div>';

    // Collect refs from data-ref attributes
    var refs = {};
    this._container.querySelectorAll('[data-ref]').forEach(function (el) {
      refs[el.dataset.ref] = el;
    });

    refs.exportBtn.addEventListener('click', function () { self._exportOtlpJson(); });

    this._refs = refs;
    this._toolRenderedCount = 0;
    this._sparkline = null;
  };

  MetricsPanel.prototype._exportOtlpJson = function () {
    var self = this;
    var spans = [];
    if (this._eventsRef) {
      this._eventsRef.forEach(function (e) {
        if (e.type === 'spanEvent') spans.push(e);
      });
    }

    var traceId = this._traceId || this._generateTraceId();
    var otlp = {
      resourceSpans: [{
        resource: {
          attributes: [
            { key: 'service.name', value: { stringValue: 'gemini-genai-examples' } },
            { key: 'session.start', value: { stringValue: new Date(self._sessionStart).toISOString() } }
          ]
        },
        scopeSpans: [{
          scope: { name: 'rs-genai-ui', version: '0.1.0' },
          spans: spans.map(function (s) {
            return {
              traceId: traceId,
              spanId: s.raw.span_id || '',
              parentSpanId: s.raw.parent_id || '',
              name: s.raw.name || '',
              kind: 1,
              startTimeUnixNano: String((self._sessionStart + s.timeMs) * 1000000),
              endTimeUnixNano: String((self._sessionStart + s.timeMs + ((s.raw.duration_us || 0) / 1000)) * 1000000),
              attributes: Object.keys(s.raw.attributes || {}).map(function (k) {
                return { key: k, value: { stringValue: String(s.raw.attributes[k]) } };
              }),
              status: { code: s.raw.status === 'ok' ? 1 : 2 }
            };
          })
        }]
      }]
    };

    var json = JSON.stringify(otlp, null, 2);
    var btn = this._refs && this._refs.exportBtn;
    navigator.clipboard.writeText(json).then(function () {
      if (btn) {
        btn.textContent = 'Copied!';
        setTimeout(function () { btn.textContent = 'Copy Trace as OTLP JSON'; }, 1500);
      }
    });
  };

  MetricsPanel.prototype._generateTraceId = function () {
    var arr = new Uint8Array(16);
    crypto.getRandomValues(arr);
    this._traceId = Array.from(arr).map(function (b) {
      return b.toString(16).padStart(2, '0');
    }).join('');
    return this._traceId;
  };

  return MetricsPanel;
})();
