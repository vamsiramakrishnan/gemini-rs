// capture-processor.js — AudioWorkletProcessor for mic capture
// Accumulates Float32 samples, converts to PCM16, posts via MessagePort.

class CaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this._buffer = new Float32Array(4096);
    this._offset = 0;
  }

  process(inputs) {
    const input = inputs[0];
    if (!input || !input[0]) return true;

    const channelData = input[0];
    let i = 0;

    while (i < channelData.length) {
      const remaining = this._buffer.length - this._offset;
      const toCopy = Math.min(remaining, channelData.length - i);

      this._buffer.set(channelData.subarray(i, i + toCopy), this._offset);
      this._offset += toCopy;
      i += toCopy;

      if (this._offset >= this._buffer.length) {
        // Convert Float32 -> PCM16
        const pcm16 = new Int16Array(this._buffer.length);
        for (let j = 0; j < this._buffer.length; j++) {
          const s = Math.max(-1, Math.min(1, this._buffer[j]));
          pcm16[j] = s < 0 ? s * 0x8000 : s * 0x7FFF;
        }

        // Transfer ownership (zero-copy)
        this.port.postMessage(pcm16, [pcm16.buffer]);
        this._offset = 0;
      }
    }

    return true;
  }
}

registerProcessor('capture-processor', CaptureProcessor);
