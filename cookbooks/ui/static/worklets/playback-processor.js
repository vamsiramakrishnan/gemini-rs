// playback-processor.js — Production AudioWorkletProcessor with adaptive jitter buffer
//
// Protocol (Main → Worklet):
//   { pcm16: Int16Array, gen: number }   Enqueue audio chunk (buffer transferred)
//   { cmd: 'flush', gen: number }        Discard buffer, advance generation fence
//
// Protocol (Worklet → Main):
//   { metrics: { depth, depthMs, state, underruns, jitterMs } }  Periodic stats
//
// Design:
//   - Generation fence: chunks tagged with gen < current are silently discarded.
//     This eliminates stale audio replay after interruption regardless of
//     message ordering or in-flight packets.
//   - Adaptive jitter buffer (RFC 6298 EWMA): accumulates a dynamic minimum depth
//     before starting playback. Measures inter-arrival jitter and adjusts fill
//     threshold accordingly. Three states: filling → playing → underrun.
//   - 2-second ring buffer (48000 samples at 24 kHz).
//   - Anti-click exponential decay on buffer drain prevents audible pops.

class PlaybackProcessor extends AudioWorkletProcessor {
  constructor() {
    super();

    // Ring buffer — 2 seconds at 24 kHz
    this._cap = 48000;
    this._ring = new Float32Array(this._cap);
    this._wr = 0;
    this._rd = 0;
    this._len = 0;

    // Generation fence — monotonically increasing; stale chunks discarded
    this._gen = 0;

    // Anti-click: last output sample for exponential decay on drain
    this._tail = 0;

    // --- Adaptive jitter buffer state ---
    // State machine: 0=filling, 1=playing, 2=underrun
    this._state = 0;

    // Minimum depth before playback starts (samples). 100ms at 24kHz = 2400.
    this._minDepth = 2400;

    // Jitter estimation (EWMA, RFC 6298 style)
    this._lastArrivalFrame = -1;     // currentFrame at last push
    this._jitterEstimate = 0;        // smoothed jitter in frames
    this._jitterAlpha = 0.125;       // EWMA factor
    this._jitterMultiple = 2.0;      // target depth = jitter * multiple

    // Underrun counter
    this._underruns = 0;

    // Metrics reporting — every ~500ms (12000 frames at 24kHz)
    this._metricFrames = 0;
    this._metricInterval = 12000;

    this.port.onmessage = (e) => {
      const d = e.data;

      // --- Flush command: clear buffer and advance generation fence ---
      if (d && d.cmd === 'flush') {
        this._gen = d.gen;
        this._wr = 0;
        this._rd = 0;
        this._len = 0;
        this._state = 0; // back to filling
        this._tail = 0;
        this._lastArrivalFrame = -1;
        return;
      }

      // --- Audio chunk with generation tag ---
      if (d && d.pcm16 !== undefined) {
        if (d.gen < this._gen) return; // stale — discard silently
        this._enqueue(d.pcm16 instanceof Int16Array ? d.pcm16 : new Int16Array(d.pcm16));
        this._updateJitter();
        this._checkFillThreshold();
        return;
      }

      // --- Legacy: bare Int16Array (no generation check) ---
      if (d instanceof Int16Array) {
        this._enqueue(d);
        this._updateJitter();
        this._checkFillThreshold();
      }
    };
  }

  _enqueue(pcm16) {
    const n = pcm16.length;
    for (let i = 0; i < n; i++) {
      if (this._len >= this._cap) break; // full — drop incoming tail
      this._ring[this._wr] = pcm16[i] / 32768.0;
      this._wr = (this._wr + 1) % this._cap;
      this._len++;
    }
  }

  _updateJitter() {
    const now = currentFrame;
    if (this._lastArrivalFrame >= 0) {
      const interval = now - this._lastArrivalFrame;
      const deviation = Math.abs(interval - this._jitterEstimate);
      this._jitterEstimate =
        this._jitterEstimate * (1.0 - this._jitterAlpha) +
        deviation * this._jitterAlpha;
    }
    this._lastArrivalFrame = now;
  }

  _adaptiveMinDepth() {
    // Dynamic minimum based on measured jitter, clamped to configured floor
    const jitterDepth = this._jitterEstimate * this._jitterMultiple;
    return Math.max(this._minDepth, Math.round(jitterDepth));
  }

  _checkFillThreshold() {
    if (this._state === 0 || this._state === 2) {
      if (this._len >= this._adaptiveMinDepth()) {
        this._state = 1; // → playing
      }
    }
  }

  process(outputs) {
    const ch = outputs[0]?.[0];
    if (!ch) return true;

    const blockLen = ch.length;

    if (this._state === 0) {
      // Filling — output silence while accumulating
      ch.fill(0);
    } else {
      // Playing or underrun-recovery
      for (let i = 0; i < blockLen; i++) {
        if (this._len > 0) {
          this._tail = this._ring[this._rd];
          ch[i] = this._tail;
          this._rd = (this._rd + 1) % this._cap;
          this._len--;
        } else if (Math.abs(this._tail) > 0.003) {
          // Anti-click: exponential decay to zero (~1.2ms at 24 kHz)
          this._tail *= 0.85;
          ch[i] = this._tail;
        } else {
          ch[i] = 0;
          this._tail = 0;
        }
      }

      // Check for underrun
      if (this._len === 0 && this._state === 1) {
        this._state = 2; // → underrun
        this._underruns++;
      }

      // Recover from underrun when buffer refills
      if (this._state === 2 && this._len >= this._adaptiveMinDepth()) {
        this._state = 1; // → playing
      }
    }

    // Periodic metrics
    this._metricFrames += blockLen;
    if (this._metricFrames >= this._metricInterval) {
      this._metricFrames = 0;
      this.port.postMessage({
        metrics: {
          depth: this._len,
          depthMs: Math.round((this._len / 24000) * 1000),
          state: this._state === 0 ? 'filling' : this._state === 1 ? 'playing' : 'underrun',
          underruns: this._underruns,
          jitterMs: Math.round((this._jitterEstimate / 24000) * 1000 * 10) / 10,
        },
      });
    }

    return true;
  }
}

registerProcessor('playback-processor', PlaybackProcessor);
