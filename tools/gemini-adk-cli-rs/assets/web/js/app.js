/**
 * app.js — Conversation UX logic + WebSocket connection
 *
 * Depends on: audio.js (AudioManager), devtools.js (DevtoolsManager)
 */

(function () {
  'use strict';

  // ------------------------------------------------
  // Extract app name from URL path: /app/{name}
  // ------------------------------------------------
  const pathParts = window.location.pathname.split('/').filter(Boolean);
  const appName = pathParts.length >= 2 ? pathParts[pathParts.length - 1] : '';

  // ------------------------------------------------
  // DOM elements
  // ------------------------------------------------
  const appTitle = document.getElementById('app-title');
  const connectionBadge = document.getElementById('connection-badge');
  const connectBtn = document.getElementById('connect-btn');
  const messagesContainer = document.getElementById('messages');
  const emptyState = document.getElementById('empty-state');
  const speakingIndicator = document.getElementById('speaking-indicator');
  const textInput = document.getElementById('text-input');
  const sendBtn = document.getElementById('send-btn');
  const micBtn = document.getElementById('mic-btn');
  const devtoolsPane = document.getElementById('devtools-pane');
  const expandBtn = document.getElementById('devtools-expand-btn');

  // ------------------------------------------------
  // Set title
  // ------------------------------------------------
  const displayName = appName.replace(/-/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
  if (appTitle) appTitle.textContent = displayName;
  document.title = displayName + ' \u2014 ADK Web UI';

  // ------------------------------------------------
  // Managers
  // ------------------------------------------------
  const audio = new AudioManager();
  const devtools = new DevtoolsManager(devtoolsPane);
  audio.onPlaybackDrained = () => {
    if (!ws || !connected) return;
    ws.send(JSON.stringify({ type: 'playbackDrained' }));
  };

  // ------------------------------------------------
  // State
  // ------------------------------------------------
  let ws = null;
  let connected = false;
  let currentModelBubble = null;
  let currentUserTranscription = null;
  let currentModelTranscription = null;
  let connectTimer = null;
  let connectStartTime = 0;
  let userScrolledUp = false;

  // ------------------------------------------------
  // Connection status
  // ------------------------------------------------
  function setConnectionState(state) {
    // state: 'disconnected' | 'connecting' | 'connected'
    connectionBadge.className = 'connection-badge ' + state;
    const dot = connectionBadge.querySelector('.dot');
    const label = connectionBadge.querySelector('.label');

    // Clear connect timer
    if (connectTimer) { clearInterval(connectTimer); connectTimer = null; }

    switch (state) {
      case 'disconnected':
        label.textContent = 'Disconnected';
        connectBtn.textContent = 'Connect';
        connectBtn.classList.remove('active');
        connectBtn.disabled = false;
        textInput.disabled = true;
        sendBtn.disabled = true;
        micBtn.disabled = true;
        connected = false;
        audio.stopRecording();
        micBtn.classList.remove('recording');
        break;
      case 'connecting':
        connectStartTime = Date.now();
        label.textContent = 'Connecting...';
        connectBtn.textContent = 'Cancel';
        connectBtn.disabled = false;
        // Show elapsed time during connection
        connectTimer = setInterval(() => {
          const elapsed = Math.floor((Date.now() - connectStartTime) / 1000);
          label.textContent = 'Connecting... ' + elapsed + 's';
        }, 1000);
        break;
      case 'connected':
        label.textContent = 'Connected';
        connectBtn.textContent = 'Disconnect';
        connectBtn.classList.add('active');
        connectBtn.disabled = false;
        textInput.disabled = false;
        sendBtn.disabled = false;
        micBtn.disabled = false;
        connected = true;
        textInput.focus();
        break;
    }
  }

  // ------------------------------------------------
  // Message rendering
  // ------------------------------------------------
  function hideEmptyState() {
    if (emptyState) emptyState.style.display = 'none';
  }

  function scrollToBottom() {
    messagesContainer.scrollTop = messagesContainer.scrollHeight;
    userScrolledUp = false;
    updateScrollBtn();
  }

  function updateScrollBtn() {
    const btn = document.getElementById('scroll-bottom-btn');
    if (!btn) return;
    btn.classList.toggle('visible', userScrolledUp);
  }

  function autoScroll() {
    if (!userScrolledUp) {
      messagesContainer.scrollTop = messagesContainer.scrollHeight;
    }
  }

  // Track if user scrolled up
  messagesContainer.addEventListener('scroll', () => {
    const threshold = 80;
    const atBottom = messagesContainer.scrollHeight - messagesContainer.scrollTop - messagesContainer.clientHeight < threshold;
    userScrolledUp = !atBottom;
    updateScrollBtn();
  });

  function addMessage(text, role) {
    hideEmptyState();

    const row = document.createElement('div');
    row.className = 'message-row ' + role;

    const bubble = document.createElement('div');
    bubble.className = 'message-bubble';
    bubble.textContent = text;

    row.appendChild(bubble);
    messagesContainer.appendChild(row);
    autoScroll();

    return bubble;
  }

  function appendToModelBubble(text) {
    hideEmptyState();

    if (!currentModelBubble) {
      currentModelBubble = addMessage('', 'model');
      currentModelBubble.parentElement.classList.add('streaming');
    }
    currentModelBubble.textContent += text;
    autoScroll();
  }

  function finalizeModelBubble() {
    if (currentModelBubble) {
      currentModelBubble.parentElement.classList.remove('streaming');
    }
    currentModelBubble = null;
    currentModelTranscription = null;
    currentUserTranscription = null;
  }

  // ------------------------------------------------
  // Transcription rendering
  // ------------------------------------------------
  function appendTranscription(role, text) {
    hideEmptyState();

    if (role === 'user') {
      if (!currentUserTranscription) {
        const row = document.createElement('div');
        row.className = 'transcription-row user';

        const bubble = document.createElement('div');
        bubble.className = 'transcription-bubble';
        bubble.innerHTML = '<span class="label">You</span> <span class="content"></span>';

        row.appendChild(bubble);
        messagesContainer.appendChild(row);
        currentUserTranscription = bubble.querySelector('.content');
      }
      currentUserTranscription.textContent = text;
    } else {
      if (!currentModelTranscription) {
        const row = document.createElement('div');
        row.className = 'transcription-row model';

        const bubble = document.createElement('div');
        bubble.className = 'transcription-bubble';
        bubble.innerHTML = '<span class="label">Assistant</span> <span class="content"></span>';

        row.appendChild(bubble);

        // Insert after the last model message row for visual grouping
        const modelRows = messagesContainer.querySelectorAll('.message-row.model');
        const lastModelRow = modelRows[modelRows.length - 1];
        if (lastModelRow && lastModelRow.nextSibling) {
          messagesContainer.insertBefore(row, lastModelRow.nextSibling);
        } else {
          messagesContainer.appendChild(row);
        }
        currentModelTranscription = bubble.querySelector('.content');
      }
      currentModelTranscription.textContent += text;
    }

    autoScroll();
  }

  // ------------------------------------------------
  // Thought rendering
  // ------------------------------------------------
  function appendThought(text) {
    hideEmptyState();

    const row = document.createElement('div');
    row.className = 'transcription-row model thought-row';

    const bubble = document.createElement('div');
    bubble.className = 'transcription-bubble thought-bubble';
    bubble.innerHTML = '<span class="label">Thinking</span> <span class="content"></span>';
    bubble.querySelector('.content').textContent = text;

    row.appendChild(bubble);
    messagesContainer.appendChild(row);
    autoScroll();
  }

  // ------------------------------------------------
  // Speaking indicator
  // ------------------------------------------------
  function setSpeaking(active) {
    if (active) {
      speakingIndicator.classList.add('active');
      currentUserTranscription = null;
      currentModelTranscription = null;
    } else {
      speakingIndicator.classList.remove('active');
    }
  }

  // ------------------------------------------------
  // WebSocket connection
  // ------------------------------------------------
  async function connect() {
    if (ws && !connected) {
      // Currently connecting — cancel
      disconnect();
      return;
    }
    if (ws) {
      // Already connected — disconnect
      disconnect();
      return;
    }

    setConnectionState('connecting');
    await audio.initPlayback();
    devtools.reset();

    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    ws = new WebSocket(`${protocol}//${window.location.host}/ws/${appName}`);

    // Connection timeout — 30 seconds
    const connectionTimeout = setTimeout(() => {
      if (!connected && ws) {
        addErrorMessage('Connection timed out after 30 seconds. Check server logs.');
        disconnect();
      }
    }, 30000);

    ws.onopen = () => {
      // Send start message
      ws.send(JSON.stringify({
        type: 'start',
        systemInstruction: null,
        model: null,
        voice: null
      }));
    };

    ws.binaryType = 'arraybuffer';

    ws.onmessage = (event) => {
      // Binary frame = raw PCM audio (hot path, no JSON overhead)
      if (event.data instanceof ArrayBuffer) {
        audio.playAudioBinary(event.data);
        return;
      }
      // Handle Blob (some browsers send binary as Blob without arraybuffer binaryType)
      if (event.data instanceof Blob) {
        event.data.arrayBuffer().then(buf => audio.playAudioBinary(buf));
        return;
      }
      // Text frame = JSON
      let msg;
      try {
        msg = JSON.parse(event.data);
      } catch (e) {
        console.error('Invalid JSON from server:', event.data);
        return;
      }

      // Forward all messages to devtools events log
      devtools.addEvent(msg);

      handleMessage(msg);
    };

    ws.onclose = () => {
      clearTimeout(connectionTimeout);
      ws = null;
      setConnectionState('disconnected');
    };

    ws.onerror = (err) => {
      clearTimeout(connectionTimeout);
      console.error('WebSocket error:', err);
    };
  }

  function disconnect() {
    if (ws) {
      ws.send(JSON.stringify({ type: 'stop' }));
      ws.close();
      ws = null;
    }
    setConnectionState('disconnected');
    finalizeModelBubble();
  }

  // ------------------------------------------------
  // Message handling
  // ------------------------------------------------
  function handleMessage(msg) {
    switch (msg.type) {
      case 'connected':
        setConnectionState('connected');
        addMessage('Session established', 'system');
        break;

      case 'textDelta':
        appendToModelBubble(msg.text);
        break;

      case 'textComplete':
        if (!currentModelBubble) {
          addMessage(msg.text, 'model');
        }
        finalizeModelBubble();
        break;

      case 'audio':
        // Text-frame audio (base64-encoded PCM16) from standalone examples
        if (msg.data) {
          audio.playAudio(msg.data);
        }
        break;

      case 'turnComplete':
        finalizeModelBubble();
        break;

      case 'interrupted':
        finalizeModelBubble();
        audio.clearQueue();
        addMessage('Model interrupted', 'system');
        break;

      case 'error':
        addErrorMessage(msg.message || 'Unknown error');
        break;

      case 'inputTranscription':
        appendTranscription('user', msg.text);
        break;

      case 'outputTranscription':
        appendTranscription('model', msg.text);
        break;

      case 'thought':
        appendThought(msg.text);
        break;

      case 'voiceActivityStart':
        setSpeaking(true);
        break;

      case 'voiceActivityEnd':
        setSpeaking(false);
        break;

      // Devtools messages
      case 'stateUpdate':
        devtools.handleStateUpdate(msg.key, msg.value);
        break;

      case 'phaseChange':
        devtools.handlePhaseChange(msg);
        addPhaseChangeMessage(msg);
        break;

      case 'evaluation':
        devtools.handleEvaluation(msg);
        break;

      case 'violation':
        devtools.handleViolation(msg);
        break;

      case 'telemetry':
        devtools.handleTelemetry(msg.stats);
        break;

      case 'phaseTimeline':
        devtools.handlePhaseTimeline(msg.entries);
        break;

      case 'toolCallEvent':
        devtools.handleToolCallEvent(msg);
        addToolCallMessage(msg);
        break;

      case 'statePromotionEvent':
        devtools.handleStatePromotionEvent(msg);
        break;

      case 'appMeta':
        devtools.handleAppMeta(msg.info);
        if (msg.info && msg.info.name) {
          const name = msg.info.name.replace(/-/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
          appTitle.textContent = name;
          document.title = name + ' \u2014 ADK Web UI';
        }
        if (msg.info && msg.info.try_saying && msg.info.try_saying.length > 0) {
          const hints = msg.info.try_saying.slice(0, 3).map(p => '\u201c' + p + '\u201d').join('  \u00b7  ');
          addMessage('Try: ' + hints, 'system');
        }
        break;

      case 'runtimeContract':
        devtools.handleRuntimeContract(msg.contract);
        break;

      case 'spanEvent':
        // Already added to timeline via devtools.addEvent(msg) above
        break;

      case 'turnMetrics':
        // Already added to timeline via devtools.addEvent(msg) above
        devtools.handleTurnMetrics(msg);
        break;

      case 'voiceRuntimeState':
        // Already added to timeline via devtools.addEvent(msg) above
        devtools.handleVoiceRuntimeState(msg);
        break;
    }
  }

  // ------------------------------------------------
  // Phase change inline display
  // ------------------------------------------------
  function addPhaseChangeMessage(msg) {
    hideEmptyState();
    const row = document.createElement('div');
    row.className = 'message-row phase-change';

    const bubble = document.createElement('div');
    bubble.className = 'message-bubble phase-change-bubble';

    const arrow = (msg.from || '?') + ' \u2192 ' + (msg.to || '?');
    const label = document.createElement('span');
    label.className = 'phase-change-label';
    label.textContent = 'Phase';

    const text = document.createElement('span');
    text.className = 'phase-change-text';
    text.textContent = arrow;

    bubble.appendChild(label);
    bubble.appendChild(text);

    if (msg.reason) {
      const reason = document.createElement('span');
      reason.className = 'phase-change-reason';
      reason.textContent = msg.reason;
      bubble.appendChild(reason);
    }

    row.appendChild(bubble);
    messagesContainer.appendChild(row);
    autoScroll();
  }

  // ------------------------------------------------
  // Tool call inline display
  // ------------------------------------------------
  function addToolCallMessage(msg) {
    hideEmptyState();
    const row = document.createElement('div');
    row.className = 'message-row tool-call';

    const bubble = document.createElement('div');
    bubble.className = 'message-bubble tool-call-bubble';

    const nameSpan = document.createElement('span');
    nameSpan.className = 'tool-call-name';
    nameSpan.textContent = msg.name || 'tool';

    const argsSpan = document.createElement('span');
    argsSpan.className = 'tool-call-args';
    const argsText = msg.args || '';
    argsSpan.textContent = argsText.length > 100 ? argsText.substring(0, 100) + '...' : argsText;

    bubble.appendChild(nameSpan);
    bubble.appendChild(argsSpan);

    if (msg.result) {
      const resultSpan = document.createElement('span');
      resultSpan.className = 'tool-call-result';
      const resultText = msg.result || '';
      resultSpan.textContent = resultText.length > 150 ? resultText.substring(0, 150) + '...' : resultText;
      bubble.appendChild(resultSpan);
    }

    row.appendChild(bubble);
    messagesContainer.appendChild(row);
    autoScroll();
  }

  // ------------------------------------------------
  // Error categorization
  // ------------------------------------------------
  function addErrorMessage(message) {
    hideEmptyState();
    const row = document.createElement('div');

    // Categorize error
    const lower = (message || '').toLowerCase();
    const isTransient = lower.includes('timeout') || lower.includes('reconnect') ||
                        lower.includes('temporarily') || lower.includes('503') ||
                        lower.includes('rate limit');
    const category = isTransient ? 'transient' : 'fatal';

    row.className = 'message-row error error-' + category;

    const bubble = document.createElement('div');
    bubble.className = 'message-bubble';

    const label = document.createElement('span');
    label.className = 'error-label';
    label.textContent = isTransient ? 'Transient' : 'Error';

    const text = document.createElement('span');
    text.className = 'error-text';
    text.textContent = message;

    bubble.appendChild(label);
    bubble.appendChild(text);
    row.appendChild(bubble);
    messagesContainer.appendChild(row);
    autoScroll();
  }

  // ------------------------------------------------
  // Send text
  // ------------------------------------------------
  function sendText() {
    const text = textInput.value.trim();
    if (!text || !connected || !ws) return;

    ws.send(JSON.stringify({ type: 'text', text }));
    addMessage(text, 'user');
    textInput.value = '';
    textInput.focus();
  }

  // ------------------------------------------------
  // Mic toggle
  // ------------------------------------------------
  async function toggleMic() {
    if (!connected || !ws) return;

    try {
      const recording = await audio.toggleRecording();
      micBtn.classList.toggle('recording', recording);
      if (recording) {
        audio.clearQueue();
      }
    } catch (err) {
      addMessage('Could not access microphone', 'error');
    }
  }

  // Audio data callback — send chunks to server
  audio.onAudioData = (base64) => {
    if (connected && ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'audio', data: base64 }));
    }
  };

  // Jitter buffer metrics → devtools
  audio.onBufferMetrics = (metrics) => {
    devtools.handleBufferMetrics(metrics);
  };

  // ------------------------------------------------
  // Devtools expand button
  // ------------------------------------------------
  if (expandBtn) {
    expandBtn.addEventListener('click', () => {
      devtools.expand();
    });
  }

  // ------------------------------------------------
  // Event listeners
  // ------------------------------------------------
  connectBtn.addEventListener('click', connect);
  sendBtn.addEventListener('click', sendText);
  micBtn.addEventListener('click', toggleMic);

  const scrollBottomBtn = document.getElementById('scroll-bottom-btn');
  if (scrollBottomBtn) {
    scrollBottomBtn.addEventListener('click', scrollToBottom);
  }

  textInput.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendText();
    }
  });

  // Initial state
  setConnectionState('disconnected');

})();
