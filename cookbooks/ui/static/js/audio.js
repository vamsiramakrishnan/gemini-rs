/**
 * audio.js — Microphone recording (PCM16 @ 16kHz) and playback (PCM16 @ 24kHz)
 *
 * Exports:
 *   AudioManager — manages mic capture and sequential audio playback
 */

class AudioManager {
  constructor() {
    /** @type {AudioContext|null} */
    this.playbackCtx = null;

    /** @type {AudioContext|null} */
    this.recordCtx = null;

    /** @type {MediaStream|null} */
    this.mediaStream = null;

    /** @type {ScriptProcessorNode|null} */
    this.scriptProcessor = null;

    this.isRecording = false;
    this.nextPlayTime = 0;

    /** @type {((base64: string) => void)|null} */
    this.onAudioData = null;
  }

  /**
   * Ensure the playback AudioContext exists (24kHz for Gemini output).
   */
  initPlayback() {
    if (!this.playbackCtx) {
      this.playbackCtx = new (window.AudioContext || window.webkitAudioContext)({
        sampleRate: 24000
      });
    }
  }

  /**
   * Play base64-encoded PCM16 audio data at 24kHz.
   * Chunks are queued sequentially to avoid overlap.
   * @param {string} base64Data
   */
  playAudio(base64Data) {
    this.initPlayback();
    if (!this.playbackCtx) return;

    // Decode base64 to binary
    const binaryString = atob(base64Data);
    const len = binaryString.length;
    const bytes = new Uint8Array(len);
    for (let i = 0; i < len; i++) {
      bytes[i] = binaryString.charCodeAt(i);
    }

    // PCM16 to Float32
    const int16 = new Int16Array(bytes.buffer);
    const float32 = new Float32Array(int16.length);
    for (let i = 0; i < int16.length; i++) {
      float32[i] = int16[i] / 32768.0;
    }

    const buffer = this.playbackCtx.createBuffer(1, float32.length, 24000);
    buffer.getChannelData(0).set(float32);

    const source = this.playbackCtx.createBufferSource();
    source.buffer = buffer;
    source.connect(this.playbackCtx.destination);

    // Schedule sequentially
    if (this.nextPlayTime < this.playbackCtx.currentTime) {
      this.nextPlayTime = this.playbackCtx.currentTime;
    }

    source.start(this.nextPlayTime);
    this.nextPlayTime += buffer.duration;
  }

  /**
   * Clear audio queue (e.g., on interruption).
   */
  clearQueue() {
    if (this.playbackCtx) {
      this.nextPlayTime = this.playbackCtx.currentTime;
    }
  }

  /**
   * Start mic recording at 16kHz mono, converting Float32 -> PCM16 -> base64.
   * Calls this.onAudioData(base64) with each chunk.
   */
  async startRecording() {
    if (this.isRecording) return;

    try {
      this.mediaStream = await navigator.mediaDevices.getUserMedia({
        audio: {
          sampleRate: 16000,
          channelCount: 1,
          echoCancellation: true,
          noiseSuppression: true
        }
      });

      this.recordCtx = new (window.AudioContext || window.webkitAudioContext)({
        sampleRate: 16000
      });

      const source = this.recordCtx.createMediaStreamSource(this.mediaStream);

      // ScriptProcessorNode (deprecated but widely supported and simple)
      this.scriptProcessor = this.recordCtx.createScriptProcessor(4096, 1, 1);

      this.scriptProcessor.onaudioprocess = (e) => {
        if (!this.isRecording || !this.onAudioData) return;

        const input = e.inputBuffer.getChannelData(0);
        const pcm16 = new Int16Array(input.length);

        for (let i = 0; i < input.length; i++) {
          const s = Math.max(-1, Math.min(1, input[i]));
          pcm16[i] = s < 0 ? s * 0x8000 : s * 0x7FFF;
        }

        // PCM16 to base64
        const uint8 = new Uint8Array(pcm16.buffer);
        let binary = '';
        for (let i = 0; i < uint8.byteLength; i++) {
          binary += String.fromCharCode(uint8[i]);
        }

        this.onAudioData(btoa(binary));
      };

      source.connect(this.scriptProcessor);
      this.scriptProcessor.connect(this.recordCtx.destination);

      this.isRecording = true;
    } catch (err) {
      console.error('Microphone access error:', err);
      throw err;
    }
  }

  /**
   * Stop mic recording and release resources.
   */
  stopRecording() {
    if (!this.isRecording) return;

    if (this.scriptProcessor) {
      this.scriptProcessor.disconnect();
      this.scriptProcessor = null;
    }

    if (this.mediaStream) {
      this.mediaStream.getTracks().forEach(track => track.stop());
      this.mediaStream = null;
    }

    if (this.recordCtx) {
      this.recordCtx.close().catch(() => {});
      this.recordCtx = null;
    }

    this.isRecording = false;
  }

  /**
   * Toggle recording state.
   * @returns {boolean} whether recording is now active
   */
  async toggleRecording() {
    if (this.isRecording) {
      this.stopRecording();
      return false;
    } else {
      await this.startRecording();
      return true;
    }
  }

  /**
   * Clean up everything.
   */
  destroy() {
    this.stopRecording();
    if (this.playbackCtx) {
      this.playbackCtx.close().catch(() => {});
      this.playbackCtx = null;
    }
  }
}
