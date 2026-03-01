const connectBtn = document.getElementById('connect-btn');
const disconnectBtn = document.getElementById('disconnect-btn');
const modelSelect = document.getElementById('model-select');
const voiceSelect = document.getElementById('voice-select');
const systemInstruction = document.getElementById('system-instruction');
const statusText = document.getElementById('status-text');
const messagesDiv = document.getElementById('messages');
const textInput = document.getElementById('text-input');
const sendBtn = document.getElementById('send-btn');
const micBtn = document.getElementById('mic-btn');

let ws = null;
let isConnected = false;
let currentBotMessageDiv = null;

// Audio context and nodes
let audioContext = null;
let recordingContext = null;
let mediaStream = null;
let scriptProcessor = null;
let nextPlayTime = 0;
let isRecording = false;

function setStatus(status) {
    if (status === 'connected') {
        statusText.textContent = 'Connected';
        statusText.className = 'status-connected';
        connectBtn.disabled = true;
        disconnectBtn.disabled = false;
        textInput.disabled = false;
        sendBtn.disabled = false;
        micBtn.disabled = false;
        modelSelect.disabled = true;
        voiceSelect.disabled = true;
        systemInstruction.disabled = true;
        isConnected = true;
    } else if (status === 'connecting') {
        statusText.textContent = 'Connecting...';
        statusText.className = 'status-connecting';
        connectBtn.disabled = true;
        disconnectBtn.disabled = true;
    } else {
        statusText.textContent = 'Disconnected';
        statusText.className = 'status-disconnected';
        connectBtn.disabled = false;
        disconnectBtn.disabled = true;
        textInput.disabled = true;
        sendBtn.disabled = true;
        micBtn.disabled = true;
        modelSelect.disabled = false;
        voiceSelect.disabled = false;
        systemInstruction.disabled = false;
        isConnected = false;
        stopRecording();
    }
}

function addMessage(text, sender) {
    const msgDiv = document.createElement('div');
    msgDiv.className = `message ${sender}`;
    msgDiv.textContent = text;
    messagesDiv.appendChild(msgDiv);
    messagesDiv.scrollTop = messagesDiv.scrollHeight;
    return msgDiv;
}

function initAudioContext() {
    if (!audioContext) {
        // Output from Gemini is 24kHz PCM16
        audioContext = new (window.AudioContext || window.webkitAudioContext)({ sampleRate: 24000 });
    }
}

connectBtn.addEventListener('click', () => {
    setStatus('connecting');
    initAudioContext();
    
    // Connect WebSocket
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    ws = new WebSocket(`${protocol}//${window.location.host}/ws`);

    ws.onopen = () => {
        // Send start config
        ws.send(JSON.stringify({
            type: 'start',
            model: modelSelect.value,
            voice: voiceSelect.value,
            system_instruction: systemInstruction.value
        }));
    };

    ws.onmessage = (event) => {
        const msg = JSON.parse(event.data);
        
        switch (msg.type) {
            case 'connected':
                setStatus('connected');
                addMessage('Session connected.', 'system');
                break;
            case 'textDelta':
                if (!currentBotMessageDiv) {
                    currentBotMessageDiv = addMessage('', 'bot');
                }
                currentBotMessageDiv.textContent += msg.text;
                messagesDiv.scrollTop = messagesDiv.scrollHeight;
                break;
            case 'textComplete':
                if (!currentBotMessageDiv) {
                    addMessage(msg.text, 'bot');
                }
                currentBotMessageDiv = null;
                break;
            case 'turnComplete':
                currentBotMessageDiv = null;
                break;
            case 'interrupted':
                currentBotMessageDiv = null;
                addMessage('Model interrupted.', 'system');
                // clear audio queue
                nextPlayTime = audioContext.currentTime;
                break;
            case 'audio':
                playAudio(msg.data);
                break;
            case 'error':
                addMessage(`Error: ${msg.message}`, 'error');
                setStatus('disconnected');
                break;
        }
    };

    ws.onclose = () => {
        setStatus('disconnected');
        addMessage('Connection closed.', 'system');
    };

    ws.onerror = (err) => {
        console.error('WebSocket error', err);
        setStatus('disconnected');
    };
});

disconnectBtn.addEventListener('click', () => {
    if (ws) {
        ws.send(JSON.stringify({ type: 'stop' }));
        ws.close();
    }
});

function sendMessage() {
    const text = textInput.value.trim();
    if (text && isConnected) {
        ws.send(JSON.stringify({ type: 'text', text }));
        addMessage(text, 'user');
        textInput.value = '';
    }
}

sendBtn.addEventListener('click', sendMessage);
textInput.addEventListener('keypress', (e) => {
    if (e.key === 'Enter') sendMessage();
});

// Audio Playback
function playAudio(base64Data) {
    if (!audioContext) return;
    
    // Decode base64 to binary
    const binaryString = window.atob(base64Data);
    const len = binaryString.length;
    const bytes = new Uint8Array(len);
    for (let i = 0; i < len; i++) {
        bytes[i] = binaryString.charCodeAt(i);
    }
    
    // PCM16 to Float32
    const int16Array = new Int16Array(bytes.buffer);
    const float32Array = new Float32Array(int16Array.length);
    for (let i = 0; i < int16Array.length; i++) {
        float32Array[i] = int16Array[i] / 32768.0;
    }
    
    const audioBuffer = audioContext.createBuffer(1, float32Array.length, 24000);
    audioBuffer.getChannelData(0).set(float32Array);
    
    const source = audioContext.createBufferSource();
    source.buffer = audioBuffer;
    source.connect(audioContext.destination);
    
    if (nextPlayTime < audioContext.currentTime) {
        nextPlayTime = audioContext.currentTime;
    }
    
    source.start(nextPlayTime);
    nextPlayTime += audioBuffer.duration;
}

// Audio Recording (Microphone)
async function toggleRecording() {
    if (isRecording) {
        stopRecording();
    } else {
        startRecording();
    }
}

async function startRecording() {
    if (!isConnected || isRecording) return;
    
    try {
        // Request microphone access at 16kHz
        mediaStream = await navigator.mediaDevices.getUserMedia({
            audio: {
                sampleRate: 16000,
                channelCount: 1,
                echoCancellation: true,
                noiseSuppression: true
            }
        });
        
        recordingContext = new (window.AudioContext || window.webkitAudioContext)({ sampleRate: 16000 });
        const source = recordingContext.createMediaStreamSource(mediaStream);
        
        // Use ScriptProcessorNode (deprecated but easy for demo)
        scriptProcessor = recordingContext.createScriptProcessor(4096, 1, 1);
        
        scriptProcessor.onaudioprocess = (e) => {
            if (!isConnected) return;
            
            const inputData = e.inputBuffer.getChannelData(0);
            const pcm16 = new Int16Array(inputData.length);
            
            for (let i = 0; i < inputData.length; i++) {
                // clamp and convert to PCM16
                let s = Math.max(-1, Math.min(1, inputData[i]));
                pcm16[i] = s < 0 ? s * 0x8000 : s * 0x7FFF;
            }
            
            // Encode to Base64
            const uint8 = new Uint8Array(pcm16.buffer);
            let binary = '';
            for (let i = 0; i < uint8.byteLength; i++) {
                binary += String.fromCharCode(uint8[i]);
            }
            const base64 = window.btoa(binary);
            
            ws.send(JSON.stringify({ type: 'audio', data: base64 }));
        };
        
        source.connect(scriptProcessor);
        scriptProcessor.connect(recordingContext.destination);
        
        isRecording = true;
        micBtn.classList.add('active');
    } catch (err) {
        console.error('Error accessing microphone:', err);
        addMessage('Error accessing microphone.', 'error');
    }
}

function stopRecording() {
    if (!isRecording) return;
    
    if (scriptProcessor) {
        scriptProcessor.disconnect();
        scriptProcessor = null;
    }
    
    if (mediaStream) {
        mediaStream.getTracks().forEach(track => track.stop());
        mediaStream = null;
    }
    
    isRecording = false;
    micBtn.classList.remove('active');
}

micBtn.addEventListener('click', toggleRecording);
