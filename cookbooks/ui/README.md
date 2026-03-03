# Gemini Live RS - UI Tester

This is a web-based UI utility to test the `gemini-live-rs` library capabilities end-to-end. It provides a simple chat interface that supports both text and real-time audio streaming (microphone input and speaker output).

## Features
- Connects to the Gemini Multimodal Live API via the `gemini-live-rs` Rust backend.
- Full-duplex WebSocket communication between the browser and the Axum server.
- Supports voice configuration and custom system instructions.
- Real-time text delta streaming.
- Real-time audio streaming (using Web Audio API for playback and recording).

## Setup & Running

1. Ensure you have your Gemini API key set in your environment variables, or create a `.env` file in the root of the project:
   ```env
   GEMINI_API_KEY=your_api_key_here
   ```

2. Run the application from the root of the repository:
   ```bash
   cargo run -p gemini-live-ui
   ```

3. Open your browser and navigate to:
   ```
   http://127.0.0.1:3000
   ```

## Usage
- Select a voice and enter any desired system instructions before connecting.
- Click **Connect** to establish the session with Gemini.
- Once connected, you can type messages and hit **Send**.
- Click the **microphone icon** (🎤) to start streaming audio from your microphone to Gemini. Click it again to stop.
- Gemini's audio responses will play automatically as they arrive.
