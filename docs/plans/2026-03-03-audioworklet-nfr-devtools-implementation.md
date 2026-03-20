# AudioWorklet + NFR Devtools Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the deprecated ScriptProcessorNode with AudioWorklet for both mic capture and speaker playback, and replace the Telemetry devtools tab with a focused NFR Metrics tab showing TTFB/latency/turn stats.

**Architecture:** Two AudioWorkletProcessor modules (capture + playback) run audio processing off the main thread. The existing Telemetry tab is replaced with a compact NFR tab that surfaces `SessionTelemetry::snapshot()` data (TTFB, per-turn latency, min/max, interruptions) plus a persistent session status bar. A ScriptProcessorNode fallback is retained for browsers without AudioWorklet support.

**Tech Stack:** Vanilla JS (AudioWorklet API, Web Audio API), existing CSS custom properties, no build step.

---

### Task 1: Create capture-processor.js AudioWorkletProcessor

**Files:**
- Create: `apps/gemini-adk-web-rs/static/worklets/capture-processor.js`

**Step 1: Create the worklets directory**

Run: `mkdir -p apps/gemini-adk-web-rs/static/worklets`

**Step 2: Write the capture processor**

```javascript
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
```

**Step 3: Commit**

```bash
git add apps/gemini-adk-web-rs/static/worklets/capture-processor.js
git commit -m "feat(ui): add capture AudioWorkletProcessor for mic input"
```

---

### Task 2: Create playback-processor.js AudioWorkletProcessor

**Files:**
- Create: `apps/gemini-adk-web-rs/static/worklets/playback-processor.js`

**Step 1: Write the playback processor**

```javascript
// playback-processor.js — AudioWorkletProcessor for speaker playback
// Ring buffer receives PCM16 chunks via MessagePort, outputs Float32.

class PlaybackProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    // Ring buffer: ~200ms at 24kHz = 4800 samples
    this._ring = new Float32Array(8192);
    this._writePos = 0;
    this._readPos = 0;
    this._count = 0; // samples available

    this.port.onmessage = (e) => {
      if (e.data && e.data.type === 'flush') {
        this._writePos = 0;
        this._readPos = 0;
        this._count = 0;
        return;
      }

      // e.data is Int16Array (PCM16)
      const pcm16 = e.data instanceof Int16Array ? e.data : new Int16Array(e.data);
      const len = pcm16.length;
      const cap = this._ring.length;

      for (let i = 0; i < len; i++) {
        if (this._count >= cap) break; // drop if full
        this._ring[this._writePos] = pcm16[i] / 32768.0;
        this._writePos = (this._writePos + 1) % cap;
        this._count++;
      }
    };
  }

  process(outputs) {
    const output = outputs[0];
    if (!output || !output[0]) return true;

    const channel = output[0];
    const cap = this._ring.length;

    for (let i = 0; i < channel.length; i++) {
      if (this._count > 0) {
        channel[i] = this._ring[this._readPos];
        this._readPos = (this._readPos + 1) % cap;
        this._count--;
      } else {
        channel[i] = 0; // underrun: silence
      }
    }

    return true;
  }
}

registerProcessor('playback-processor', PlaybackProcessor);
```

**Step 2: Commit**

```bash
git add apps/gemini-adk-web-rs/static/worklets/playback-processor.js
git commit -m "feat(ui): add playback AudioWorkletProcessor with ring buffer"
```

---

### Task 3: Rewrite AudioManager with worklet support + fallback

**Files:**
- Modify: `apps/gemini-adk-web-rs/static/js/audio.js` (full rewrite)

**Step 1: Rewrite audio.js**

Replace the entire file with an AudioManager that:
1. Feature-detects `AudioWorkletNode`
2. Uses worklet processors when available
3. Falls back to ScriptProcessorNode for capture and AudioBufferSourceNode for playback
4. Uses `Transferable` ArrayBuffers for zero-copy MessagePort communication

```javascript
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
      // Worklet path: send PCM16 to ring buffer (transfer ownership)
      const copy = new Int16Array(int16);
      this._playbackNode.port.postMessage(copy, [copy.buffer]);
    } else {
      // Fallback: schedule AudioBufferSourceNode
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
    }
  }

  clearQueue() {
    if (this._playbackNode) {
      this._playbackNode.port.postMessage({ type: 'flush' });
    } else if (this.playbackCtx) {
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
        // Worklet doesn't need to connect to destination
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
```

**Step 2: Update app.js to call initPlayback with await**

In `apps/gemini-adk-web-rs/static/js/app.js`, change `audio.initPlayback()` (line 205) to `await audio.initPlayback()` and make `connect()` async:

Change line 197: `function connect() {` -> `async function connect() {`
Change line 205: `audio.initPlayback();` -> `await audio.initPlayback();`

**Step 3: Verify worklet files are served by the static file server**

The Axum `ServeDir` at `/static` already serves everything under `apps/gemini-adk-web-rs/static/`, so `/static/worklets/capture-processor.js` will be accessible automatically.

**Step 4: Commit**

```bash
git add apps/gemini-adk-web-rs/static/js/audio.js apps/gemini-adk-web-rs/static/js/app.js
git commit -m "feat(ui): rewrite AudioManager with AudioWorklet + ScriptProcessorNode fallback"
```

---

### Task 4: Add session status bar to devtools

**Files:**
- Modify: `apps/gemini-adk-web-rs/static/app.html` (add status bar container)
- Modify: `apps/gemini-adk-web-rs/static/css/devtools.css` (add status bar styles)
- Modify: `apps/gemini-adk-web-rs/static/js/devtools.js` (add status bar logic)

**Step 1: Add status bar HTML to app.html**

After the devtools tab bar `<div class="devtools-tabs">...</div>` (line 96), add:

```html
        <!-- Session status bar -->
        <div class="devtools-status-bar" id="devtools-status-bar">
          <span class="status-item status-uptime" id="status-uptime">--</span>
          <span class="status-separator">|</span>
          <span class="status-item">Phase: <span class="status-phase" id="status-phase">--</span></span>
          <span class="status-separator">|</span>
          <span class="status-item">Turns: <span class="status-turns" id="status-turns">0</span></span>
        </div>
```

**Step 2: Add status bar CSS to devtools.css**

Append to the end of `devtools.css`:

```css
/* ============================================
   Session Status Bar
   ============================================ */
.devtools-status-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 12px;
  background: #f8f9fa;
  border-bottom: 1px solid var(--border-light);
  font-size: 11px;
  font-family: var(--mono);
  color: var(--text-secondary);
  min-height: 24px;
}

.status-separator {
  color: var(--border);
}

.status-phase {
  font-weight: 600;
  color: var(--primary-dark);
  padding: 1px 5px;
  background: var(--primary-light);
  border-radius: 3px;
  font-size: 10px;
}

.status-turns {
  font-weight: 600;
  color: var(--text-primary);
}

.status-uptime {
  font-weight: 500;
}
```

**Step 3: Update DevtoolsManager to track status bar**

In `devtools.js`, add status bar update logic:

1. In `constructor()`, after `this.sessionStart = Date.now();`, add:
   ```javascript
   this._statusUptimeEl = null;
   this._statusPhaseEl = null;
   this._statusTurnsEl = null;
   this._statusRafId = null;
   ```

2. Add a new method `_initStatusBar()` called from constructor:
   ```javascript
   _initStatusBar() {
     this._statusUptimeEl = document.getElementById('status-uptime');
     this._statusPhaseEl = document.getElementById('status-phase');
     this._statusTurnsEl = document.getElementById('status-turns');
   }
   ```

3. Add `_startStatusTicker()` and `_stopStatusTicker()`:
   ```javascript
   _startStatusTicker() {
     const tick = () => {
       if (this._statusUptimeEl) {
         const elapsed = Date.now() - this.sessionStart;
         this._statusUptimeEl.textContent = this._formatElapsed(elapsed);
       }
       this._statusRafId = requestAnimationFrame(tick);
     };
     this._statusRafId = requestAnimationFrame(tick);
   }

   _stopStatusTicker() {
     if (this._statusRafId) {
       cancelAnimationFrame(this._statusRafId);
       this._statusRafId = null;
     }
   }
   ```

4. Update `handleTelemetry()` to also update status bar:
   ```javascript
   // After existing telemetry update logic, add:
   if (stats.current_phase && this._statusPhaseEl) {
     this._statusPhaseEl.textContent = stats.current_phase;
   }
   if (stats.turn_count !== undefined && this._statusTurnsEl) {
     this._statusTurnsEl.textContent = stats.turn_count;
   }
   ```

5. Call `_initStatusBar()` from constructor, `_startStatusTicker()` after first telemetry, `_stopStatusTicker()` in `reset()`.

**Step 4: Commit**

```bash
git add apps/gemini-adk-web-rs/static/app.html apps/gemini-adk-web-rs/static/css/devtools.css apps/gemini-adk-web-rs/static/js/devtools.js
git commit -m "feat(ui): add session status bar to devtools (uptime, phase, turns)"
```

---

### Task 5: Replace Telemetry tab with NFR Metrics tab

**Files:**
- Modify: `apps/gemini-adk-web-rs/static/js/devtools.js`
- Modify: `apps/gemini-adk-web-rs/static/css/devtools.css`

**Step 1: Rename telemetry panel to NFR panel**

In `devtools.js`:

1. In `_initPanels()`, change the telemetry panel setup:
   - Change `id = 'panel-telemetry'` -> `id = 'panel-nfr'`
   - Change `this.panels.telemetry` -> `this.panels.nfr`

2. In `_tabLabel()`, replace the telemetry case:
   ```javascript
   case 'nfr': return 'NFR';
   ```
   Remove: `case 'telemetry': return 'Telemetry';`

3. In `handleAppMeta()`, replace the telemetry tab logic:
   ```javascript
   // Always show NFR tab
   this.availableTabs.push('nfr');
   ```
   Remove the old `if (info.category === 'advanced' || info.category === 'showcase')` block for telemetry.

4. In `reset()`, change:
   - `this.panels.telemetry.innerHTML = ...` -> `this.panels.nfr.innerHTML = ...`

**Step 2: Rewrite `_renderTelemetry()` as `_renderNfr()`**

Replace the entire `_renderTelemetry()` method with a focused NFR metrics renderer:

```javascript
_renderNfr() {
  const panel = this.panels.nfr;
  const stats = this.telemetry;

  if (!stats || Object.keys(stats).length === 0) {
    panel.innerHTML = '<div class="events-empty">No metrics yet</div>';
    return;
  }

  let html = '<div class="nfr-content">';

  // TTFB Section
  if (stats.response_count > 0) {
    const last = stats.last_response_latency_ms || 0;
    const avg = stats.avg_response_latency_ms || 0;
    const cls = (v) => v < 300 ? 'nfr-good' : v < 600 ? 'nfr-ok' : 'nfr-warn';

    html += `<div class="nfr-section">
      <div class="nfr-section-title">TTFB (Time to First Byte)</div>
      <div class="nfr-grid">
        <div class="nfr-stat">
          <div class="nfr-stat-value ${cls(last)}">${last}<span class="nfr-unit">ms</span></div>
          <div class="nfr-stat-label">Last</div>
        </div>
        <div class="nfr-stat">
          <div class="nfr-stat-value ${cls(avg)}">${avg}<span class="nfr-unit">ms</span></div>
          <div class="nfr-stat-label">Avg</div>
        </div>
        <div class="nfr-stat">
          <div class="nfr-stat-value">${stats.response_count}</div>
          <div class="nfr-stat-label">Responses</div>
        </div>
      </div>`;

    // Min/max range
    if (stats.response_count > 1) {
      html += `<div class="nfr-range">
        <span class="nfr-range-label">min</span>
        <span class="nfr-range-value">${stats.min_response_latency_ms || 0}ms</span>
        <span class="nfr-range-bar"></span>
        <span class="nfr-range-value">${stats.max_response_latency_ms || 0}ms</span>
        <span class="nfr-range-label">max</span>
      </div>`;
    }

    html += '</div>';
  }

  // Turn Duration Section
  html += `<div class="nfr-section">
    <div class="nfr-section-title">Per-Turn Duration</div>
    <div class="nfr-grid">`;

  if (stats.avg_turn_duration_ms > 0) {
    const secs = (stats.avg_turn_duration_ms / 1000).toFixed(1);
    html += `<div class="nfr-stat">
        <div class="nfr-stat-value">${secs}<span class="nfr-unit">s</span></div>
        <div class="nfr-stat-label">Avg Turn</div>
      </div>`;
  }

  html += `<div class="nfr-stat">
      <div class="nfr-stat-value">${stats.interruptions || 0}</div>
      <div class="nfr-stat-label">Interruptions</div>
    </div>
  </div></div>`;

  // Audio Throughput Section
  if (stats.audio_chunks_out > 0) {
    html += `<div class="nfr-section">
      <div class="nfr-section-title">Audio</div>
      <div class="nfr-grid">
        <div class="nfr-stat">
          <div class="nfr-stat-value">${stats.audio_kbytes_out || 0}<span class="nfr-unit">KB</span></div>
          <div class="nfr-stat-label">Total</div>
        </div>
        <div class="nfr-stat">
          <div class="nfr-stat-value">${stats.audio_throughput_kbps || 0}<span class="nfr-unit">KB/s</span></div>
          <div class="nfr-stat-label">Throughput</div>
        </div>
        <div class="nfr-stat">
          <div class="nfr-stat-value">${stats.uptime_secs || 0}<span class="nfr-unit">s</span></div>
          <div class="nfr-stat-label">Uptime</div>
        </div>
      </div>
    </div>`;
  }

  // Tool Calls Section (kept — useful for NFR)
  if (this.toolCalls.length > 0) {
    html += `<div class="nfr-section">
      <div class="nfr-section-title">Tool Calls (${this.toolCalls.length})</div>
      <div class="nfr-tool-list">`;

    this.toolCalls.slice(-5).forEach(tc => {
      html += `<div class="nfr-tool-entry">
        <span class="nfr-tool-name">${this._esc(tc.name)}</span>
        <span class="nfr-tool-args">${this._truncate(tc.args, 40)}</span>
      </div>`;
    });

    html += '</div></div>';
  }

  html += '</div>';
  panel.innerHTML = html;
}
```

**Step 3: Update all references**

- `handleTelemetry()`: change `this._renderTelemetry()` -> `this._renderNfr()`
- `handleToolCallEvent()`: change `this._renderTelemetry()` -> `this._renderNfr()`

**Step 4: Add NFR CSS styles**

In `devtools.css`, replace the old telemetry styles (lines 596-769) with:

```css
/* ============================================
   NFR Metrics Panel
   ============================================ */
.nfr-content {
  padding: 12px;
}

.nfr-section {
  margin-bottom: 16px;
}

.nfr-section-title {
  font-size: 10px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--text-muted);
  margin-bottom: 8px;
  padding-bottom: 4px;
  border-bottom: 1px solid var(--border-light);
}

.nfr-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 8px;
}

.nfr-stat {
  text-align: center;
  padding: 8px 4px;
  background: #ffffff;
  border: 1px solid var(--border-light);
  border-radius: 6px;
}

.nfr-stat-value {
  font-size: 18px;
  font-weight: 700;
  color: var(--text-primary);
  font-family: var(--mono);
  line-height: 1.2;
}

.nfr-stat-value.nfr-good { color: #1b873f; }
.nfr-stat-value.nfr-ok { color: #d4820c; }
.nfr-stat-value.nfr-warn { color: var(--error); }

.nfr-unit {
  font-size: 10px;
  font-weight: 500;
  color: var(--text-muted);
  margin-left: 1px;
}

.nfr-stat-label {
  font-size: 10px;
  font-weight: 500;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--text-muted);
  margin-top: 2px;
}

.nfr-range {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 8px;
  font-size: 10px;
  font-family: var(--mono);
  color: var(--text-secondary);
  margin-top: 4px;
}

.nfr-range-label {
  font-weight: 500;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--text-muted);
  font-size: 9px;
}

.nfr-range-value {
  font-weight: 600;
  color: var(--text-primary);
}

.nfr-range-bar {
  flex: 1;
  height: 2px;
  background: linear-gradient(to right, #1b873f, #d4820c, var(--error));
  border-radius: 1px;
  opacity: 0.5;
}

.nfr-tool-list {
  margin-top: 6px;
}

.nfr-tool-entry {
  display: flex;
  gap: 6px;
  padding: 4px 8px;
  font-size: 11px;
  font-family: var(--mono);
  background: #ffffff;
  border: 1px solid var(--border-light);
  border-radius: 4px;
  margin-bottom: 4px;
  align-items: center;
}

.nfr-tool-name {
  font-weight: 600;
  color: var(--primary-dark);
  flex-shrink: 0;
}

.nfr-tool-args {
  color: var(--text-secondary);
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
```

**Step 5: Commit**

```bash
git add apps/gemini-adk-web-rs/static/js/devtools.js apps/gemini-adk-web-rs/static/css/devtools.css
git commit -m "feat(ui): replace Telemetry tab with focused NFR Metrics tab"
```

---

### Task 6: Verify build and manual test

**Step 1: Run cargo check**

Run: `cargo check -p gemini-genai-ui`
Expected: Compilation succeeds (no Rust changes in this workstream).

**Step 2: Run cargo test**

Run: `cargo test -p gemini-genai-ui`
Expected: All existing tests pass (UI changes don't affect Rust tests).

**Step 3: Manual browser verification checklist**

Start the server: `cargo run -p gemini-genai-ui`

Verify in browser at `http://localhost:25125`:
- [ ] Landing page loads, app cards show
- [ ] Click any app -> app page loads
- [ ] Devtools panel shows State, Events, NFR tabs (no "Telemetry" tab)
- [ ] Session status bar shows above tabs with "--" placeholders
- [ ] Click Connect -> session starts
- [ ] Status bar updates with uptime (ticking), phase name, turn count
- [ ] NFR tab shows TTFB metrics after first voice interaction
- [ ] Microphone works (AudioWorklet in Chrome, fallback in Firefox ESR)
- [ ] Audio playback works without clicks or gaps
- [ ] Interruption (speak while model is speaking) clears audio queue

**Step 4: Commit any fixes and final commit**

```bash
git add -A
git commit -m "chore(ui): final polish for AudioWorklet + NFR devtools"
```
