// playback-processor.js — Production AudioWorkletProcessor for speech playback
//
// Protocol (Main → Worklet):
//   { pcm16: Int16Array, gen: number }   Enqueue audio chunk (buffer transferred)
//   { cmd: 'flush', gen: number }        Discard buffer, advance generation fence
//
// Design:
//   - Generation fence: chunks tagged with gen < current are silently discarded.
//     This eliminates stale audio replay after interruption regardless of
//     message ordering or in-flight packets.
//   - 1-second ring buffer (24000 samples at 24 kHz) — large enough to absorb
//     network jitter bursts without dropping samples.
//   - Anti-click exponential decay on buffer drain prevents audible pops when
//     playback stops abruptly (interruption or natural end-of-turn).

class PlaybackProcessor extends AudioWorkletProcessor {
  constructor() {
    super();

    // Ring buffer — 1 second at 24 kHz
    this._cap = 24000;
    this._ring = new Float32Array(this._cap);
    this._wr = 0;
    this._rd = 0;
    this._len = 0;

    // Generation fence — monotonically increasing; stale chunks discarded
    this._gen = 0;

    // Anti-click: last output sample for exponential decay on drain
    this._tail = 0;

    this.port.onmessage = (e) => {
      const d = e.data;

      // --- Flush command: clear buffer and advance generation fence ---
      if (d && d.cmd === 'flush') {
        this._gen = d.gen;
        this._wr = 0;
        this._rd = 0;
        this._len = 0;
        return;
      }

      // --- Audio chunk with generation tag ---
      if (d && d.pcm16 !== undefined) {
        if (d.gen < this._gen) return; // stale — discard silently
        this._enqueue(d.pcm16 instanceof Int16Array ? d.pcm16 : new Int16Array(d.pcm16));
        return;
      }

      // --- Legacy: bare Int16Array (no generation check) ---
      if (d instanceof Int16Array) {
        this._enqueue(d);
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

  process(outputs) {
    const ch = outputs[0]?.[0];
    if (!ch) return true;

    for (let i = 0; i < ch.length; i++) {
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

    return true;
  }
}

registerProcessor('playback-processor', PlaybackProcessor);
