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
  document.title = displayName + ' \u2014 Cookbooks';

  // ------------------------------------------------
  // Managers
  // ------------------------------------------------
  const audio = new AudioManager();
  const devtools = new DevtoolsManager(devtoolsPane);

  // ------------------------------------------------
  // State
  // ------------------------------------------------
  let ws = null;
  let connected = false;
  let currentModelBubble = null;
  let currentUserTranscription = null;
  let currentModelTranscription = null;

  // ------------------------------------------------
  // Connection status
  // ------------------------------------------------
  function setConnectionState(state) {
    // state: 'disconnected' | 'connecting' | 'connected'
    connectionBadge.className = 'connection-badge ' + state;
    const dot = connectionBadge.querySelector('.dot');
    const label = connectionBadge.querySelector('.label');

    switch (state) {
      case 'disconnected':
        label.textContent = 'Disconnected';
        connectBtn.textContent = 'Connect';
        connectBtn.classList.remove('active');
        textInput.disabled = true;
        sendBtn.disabled = true;
        micBtn.disabled = true;
        connected = false;
        audio.stopRecording();
        micBtn.classList.remove('recording');
        break;
      case 'connecting':
        label.textContent = 'Connecting';
        connectBtn.textContent = 'Connecting...';
        connectBtn.disabled = true;
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
        break;
    }
  }

  // ------------------------------------------------
  // Message rendering
  // ------------------------------------------------
  function hideEmptyState() {
    if (emptyState) emptyState.style.display = 'none';
  }

  function addMessage(text, role) {
    hideEmptyState();

    const row = document.createElement('div');
    row.className = 'message-row ' + role;

    const bubble = document.createElement('div');
    bubble.className = 'message-bubble';
    bubble.textContent = text;

    row.appendChild(bubble);
    messagesContainer.appendChild(row);
    messagesContainer.scrollTop = messagesContainer.scrollHeight;

    return bubble;
  }

  function appendToModelBubble(text) {
    hideEmptyState();

    if (!currentModelBubble) {
      currentModelBubble = addMessage('', 'model');
      currentModelBubble.parentElement.classList.add('streaming');
    }
    currentModelBubble.textContent += text;
    messagesContainer.scrollTop = messagesContainer.scrollHeight;
  }

  function finalizeModelBubble() {
    if (currentModelBubble) {
      currentModelBubble.parentElement.classList.remove('streaming');
    }
    currentModelBubble = null;
    currentModelTranscription = null;
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

    messagesContainer.scrollTop = messagesContainer.scrollHeight;
  }

  // ------------------------------------------------
  // Speaking indicator
  // ------------------------------------------------
  function setSpeaking(active) {
    if (active) {
      speakingIndicator.classList.add('active');
      currentUserTranscription = null; // reset for new speech
    } else {
      speakingIndicator.classList.remove('active');
    }
  }

  // ------------------------------------------------
  // WebSocket connection
  // ------------------------------------------------
  async function connect() {
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

    ws.onopen = () => {
      // Send start message
      ws.send(JSON.stringify({
        type: 'start',
        systemInstruction: null,
        model: null,
        voice: null
      }));
    };

    ws.onmessage = (event) => {
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
      ws = null;
      setConnectionState('disconnected');
    };

    ws.onerror = (err) => {
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
        audio.playAudio(msg.data);
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
        addMessage(msg.message || 'Unknown error', 'error');
        break;

      case 'inputTranscription':
        appendTranscription('user', msg.text);
        break;

      case 'outputTranscription':
        appendTranscription('model', msg.text);
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
        break;

      case 'appMeta':
        devtools.handleAppMeta(msg.info);
        if (msg.info && msg.info.name) {
          const name = msg.info.name.replace(/-/g, ' ').replace(/\b\w/g, c => c.toUpperCase());
          appTitle.textContent = name;
          document.title = name + ' \u2014 Cookbooks';
        }
        if (msg.info && msg.info.try_saying && msg.info.try_saying.length > 0) {
          const hints = msg.info.try_saying.slice(0, 3).map(p => '\u201c' + p + '\u201d').join('  \u00b7  ');
          addMessage('Try: ' + hints, 'system');
        }
        break;

      case 'spanEvent':
        // Already added to timeline via devtools.addEvent(msg) above
        break;

      case 'turnMetrics':
        // Already added to timeline via devtools.addEvent(msg) above
        devtools.handleTurnMetrics(msg);
        break;
    }
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

  textInput.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendText();
    }
  });

  // Initial state
  setConnectionState('disconnected');

})();
