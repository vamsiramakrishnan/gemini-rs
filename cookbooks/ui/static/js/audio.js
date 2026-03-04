/**
 * audio.js — High-performance audio I/O via AudioWorklet with ScriptProcessorNode fallback
 *
 * Recording: 16kHz mono PCM16 -> base64
 * Playback:  base64 -> PCM16 @ 24kHz
 */

class AudioManager {
  constructor() {
    this.playbackCtx = null;
    this.recordCtx = null;
    this.mediaStream = null;
    this.isRecording = false;
    this.onAudioData = null;

    // Worklet nodes (when supported)
    this._captureNode = null;
    this._playbackNode = null;

    // Legacy fallback references
    this._scriptProcessor = null;
    this._nextPlayTime = 0;
    this._activeSources = [];

    // Generation fence — incremented on every clearQueue().
    // Audio tagged with a stale generation is silently discarded by the worklet.
    this._generation = 0;

    this._workletSupported = typeof AudioWorkletNode !== 'undefined';
  }

  // --- Playback ---

  async initPlayback() {
    if (this.playbackCtx) return;

    this.playbackCtx = new (window.AudioContext || window.webkitAudioContext)({
      sampleRate: 24000,
    });

    if (this._workletSupported) {
      try {
        await this.playbackCtx.addModule('/static/worklets/playback-processor.js');
        this._playbackNode = new AudioWorkletNode(this.playbackCtx, 'playback-processor');
        this._playbackNode.connect(this.playbackCtx.destination);
      } catch (err) {
        console.warn('Playback worklet failed, using fallback:', err);
        this._playbackNode = null;
      }
    }
  }

  playAudio(base64Data) {
    if (!this.playbackCtx) return;

    // Decode base64 -> binary -> Int16Array
    const binaryString = atob(base64Data);
    const len = binaryString.length;
    const bytes = new Uint8Array(len);
    for (let i = 0; i < len; i++) {
      bytes[i] = binaryString.charCodeAt(i);
    }
    const int16 = new Int16Array(bytes.buffer);

    if (this._playbackNode) {
      // Worklet path: send PCM16 with generation fence (transfer ownership)
      const copy = new Int16Array(int16);
      this._playbackNode.port.postMessage(
        { pcm16: copy, gen: this._generation },
        [copy.buffer]
      );
    } else {
      // Fallback: schedule AudioBufferSourceNode with cancellation tracking
      const float32 = new Float32Array(int16.length);
      for (let i = 0; i < int16.length; i++) {
        float32[i] = int16[i] / 32768.0;
      }

      const buffer = this.playbackCtx.createBuffer(1, float32.length, 24000);
      buffer.getChannelData(0).set(float32);

      const source = this.playbackCtx.createBufferSource();
      source.buffer = buffer;
      source.connect(this.playbackCtx.destination);

      if (this._nextPlayTime < this.playbackCtx.currentTime) {
        this._nextPlayTime = this.playbackCtx.currentTime;
      }
      source.start(this._nextPlayTime);
      this._nextPlayTime += buffer.duration;

      // Track for cancellation on interruption
      this._activeSources.push(source);
      source.onended = () => {
        const idx = this._activeSources.indexOf(source);
        if (idx !== -1) this._activeSources.splice(idx, 1);
      };
    }
  }

  clearQueue() {
    // Advance generation — the worklet will silently discard any audio
    // tagged with a prior generation, including chunks already in the
    // postMessage queue that haven't been processed yet.
    this._generation++;

    if (this._playbackNode) {
      this._playbackNode.port.postMessage({ cmd: 'flush', gen: this._generation });
    }

    // Fallback: stop all scheduled AudioBufferSourceNodes immediately.
    // Without this, already-scheduled sources play to completion and
    // overlap with the next turn's audio (the "played twice" bug).
    for (const src of this._activeSources) {
      try { src.stop(); } catch (_) {}
    }
    this._activeSources = [];

    if (this.playbackCtx) {
      this._nextPlayTime = this.playbackCtx.currentTime;
    }
  }

  // --- Recording ---

  async startRecording() {
    if (this.isRecording) return;

    this.mediaStream = await navigator.mediaDevices.getUserMedia({
      audio: {
        sampleRate: 16000,
        channelCount: 1,
        echoCancellation: true,
        noiseSuppression: true,
      },
    });

    this.recordCtx = new (window.AudioContext || window.webkitAudioContext)({
      sampleRate: 16000,
    });

    const source = this.recordCtx.createMediaStreamSource(this.mediaStream);

    if (this._workletSupported) {
      try {
        await this.recordCtx.addModule('/static/worklets/capture-processor.js');
        this._captureNode = new AudioWorkletNode(this.recordCtx, 'capture-processor');
        this._captureNode.port.onmessage = (e) => {
          if (!this.isRecording || !this.onAudioData) return;
          // e.data is Int16Array (transferred from worklet)
          this.onAudioData(this._int16ToBase64(e.data));
        };
        source.connect(this._captureNode);
      } catch (err) {
        console.warn('Capture worklet failed, using ScriptProcessorNode fallback:', err);
        this._startRecordingFallback(source);
      }
    } else {
      this._startRecordingFallback(source);
    }

    this.isRecording = true;
  }

  _startRecordingFallback(source) {
    this._scriptProcessor = this.recordCtx.createScriptProcessor(4096, 1, 1);
    this._scriptProcessor.onaudioprocess = (e) => {
      if (!this.isRecording || !this.onAudioData) return;

      const input = e.inputBuffer.getChannelData(0);
      const pcm16 = new Int16Array(input.length);
      for (let i = 0; i < input.length; i++) {
        const s = Math.max(-1, Math.min(1, input[i]));
        pcm16[i] = s < 0 ? s * 0x8000 : s * 0x7FFF;
      }
      this.onAudioData(this._int16ToBase64(pcm16));
    };
    source.connect(this._scriptProcessor);
    this._scriptProcessor.connect(this.recordCtx.destination);
  }

  stopRecording() {
    if (!this.isRecording) return;

    if (this._captureNode) {
      this._captureNode.disconnect();
      this._captureNode = null;
    }
    if (this._scriptProcessor) {
      this._scriptProcessor.disconnect();
      this._scriptProcessor = null;
    }
    if (this.mediaStream) {
      this.mediaStream.getTracks().forEach((t) => t.stop());
      this.mediaStream = null;
    }
    if (this.recordCtx) {
      this.recordCtx.close().catch(() => {});
      this.recordCtx = null;
    }
    this.isRecording = false;
  }

  async toggleRecording() {
    if (this.isRecording) {
      this.stopRecording();
      return false;
    }
    await this.startRecording();
    return true;
  }

  destroy() {
    this.stopRecording();
    this.clearQueue();
    if (this._playbackNode) {
      this._playbackNode.disconnect();
      this._playbackNode = null;
    }
    if (this.playbackCtx) {
      this.playbackCtx.close().catch(() => {});
      this.playbackCtx = null;
    }
  }

  // --- Helpers ---

  _int16ToBase64(int16) {
    const uint8 = new Uint8Array(int16.buffer);
    let binary = '';
    for (let i = 0; i < uint8.byteLength; i++) {
      binary += String.fromCharCode(uint8[i]);
    }
    return btoa(binary);
  }
}
