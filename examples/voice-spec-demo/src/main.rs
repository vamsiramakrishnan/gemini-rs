//! A whole phone call, driven by one JSON document and two models.
//!
//! `spec.json` is the restaurant cookbook from the Flow Studio gallery,
//! edited by hand (audio modality, a named voice, memory section removed) —
//! the point being that the document the Studio edits is just JSON you can
//! edit too. This binary:
//!
//! 1. loads and validates the spec, replays its embedded tests offline,
//! 2. applies it to a Live session (`SessionSpec::apply`) — governed flow,
//!    mock tools, computed state, watchers, runtime tuning all come from
//!    the document,
//! 3. plays the caller using **Gemini TTS** (`generateContent` with an
//!    AUDIO response modality),
//! 4. bridges caller audio into the session through `voice::pump` — the
//!    same device-independent duplex core `talk()` and telephony use,
//! 5. records both sides into `voice-spec-demo.wav` and prints the
//!    transcript with the flow's live state after every turn.
//!
//! Run with `GEMINI_API_KEY` set:
//!
//! ```bash
//! cargo run -p example-voice-spec-demo
//! ```

use std::time::Duration;

use base64::Engine as _;
use gemini_adk_fluent_rs::live::LiveEvent;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::spec::{SessionSpec, SpecResources};
use gemini_adk_fluent_rs::voice::{Playback, pump};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};

const LIVE_MODEL: &str = "models/gemini-2.5-flash-native-audio-preview-12-2025";
const TTS_MODEL: &str = "models/gemini-2.5-flash-preview-tts";
const CALLER_VOICE: &str = "Zephyr";
const RATE: u32 = 24_000;

const CALLER_LINES: &[&str] = &[
    "Hi! I'd like a table for two tomorrow at seven in the evening.",
    "Seven thirty works perfectly. Please book it.",
    "That's everything. Thank you so much, goodbye!",
];

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GEMINI_API_KEY")?;

    // ── 1. The document ─────────────────────────────────────────────────
    let spec: SessionSpec = serde_json::from_str(include_str!("../spec.json"))?;
    let validation = spec.validate();
    println!("spec `{}` valid: {}", spec.name, validation.valid);
    for w in &validation.warnings {
        println!("  warning: {w}");
    }
    for report in gemini_adk_fluent_rs::spec::run_tests(&spec) {
        println!(
            "  embedded test `{}`: {} ({} events)",
            report.name,
            if report.passed { "passed" } else { "FAILED" },
            report.events
        );
    }

    // ── 2. The session, configured entirely from the document ───────────
    let state = State::new();
    let live = Live::builder()
        .model(ModelId::new(LIVE_MODEL))
        .transcription(true, true);
    let live = spec
        .apply(live, &state, &SpecResources::default())
        .map_err(|e| format!("spec.apply: {e}"))?;
    let handle = live.connect_from_env().await?;
    println!("connected: {LIVE_MODEL}");

    // ── 3+4. Caller TTS in, agent audio out, both through voice::pump ───
    let (mic_tx, mic_rx) = mpsc::channel::<Vec<i16>>(256);
    let (spk_tx, mut spk_rx) = mpsc::channel::<Playback>(256);
    let _pump = pump(&handle, mic_rx, RATE, spk_tx, RATE);

    let mut events = handle.events();
    let http = reqwest::Client::new();
    let mut wav: Vec<i16> = Vec::new();

    for (turn, line) in CALLER_LINES.iter().enumerate() {
        // Record the agent until its turn completes and its audio drains.
        record_agent_turn(&mut events, &mut spk_rx, &mut wav).await;
        print_flow_state(&handle);

        // The caller speaks: synthesize, record, and stream into the mic
        // channel at real-time pace so server-side VAD hears natural speech.
        println!("[caller] {line}");
        let samples = tts(&http, &api_key, line).await?;
        wav.extend_from_slice(&samples);
        stream_as_mic(&mic_tx, &samples).await;
        if turn + 1 == CALLER_LINES.len() {
            // Let the model answer the goodbye before hanging up.
            record_agent_turn(&mut events, &mut spk_rx, &mut wav).await;
            print_flow_state(&handle);
        }
    }

    write_wav("voice-spec-demo.wav", &wav, RATE)?;
    println!(
        "wrote voice-spec-demo.wav — {:.1}s of call audio",
        wav.len() as f64 / RATE as f64
    );
    handle.disconnect().await?;
    Ok(())
}

/// Drain agent audio into the recording until the turn boundary passes and
/// the playback channel has been quiet long enough to be sure it drained.
async fn record_agent_turn(
    events: &mut tokio::sync::broadcast::Receiver<LiveEvent>,
    spk_rx: &mut mpsc::Receiver<Playback>,
    wav: &mut Vec<i16>,
) {
    let mut turn_done = false;
    let mut quiet = tokio::time::Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        tokio::select! {
            playback = spk_rx.recv() => match playback {
                Some(Playback::Chunk(samples)) => { wav.extend_from_slice(&samples); quiet = tokio::time::Instant::now(); }
                Some(Playback::Flush) | None => {}
            },
            event = events.recv() => match event {
                Ok(LiveEvent::TurnComplete) => turn_done = true,
                Ok(LiveEvent::OutputTranscript { text, is_final: true }) => println!("[agent] {text}"),
                Ok(LiveEvent::InputTranscript { text, is_final: true }) => println!("[heard] {text}"),
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                _ => {}
            },
            () = sleep(Duration::from_millis(200)) => {}
        }
        let quiet_ms = quiet.elapsed().as_millis();
        if (turn_done && quiet_ms > 1200) || tokio::time::Instant::now() > deadline {
            // Half a second of silence between speakers reads as natural.
            wav.extend(std::iter::repeat_n(0i16, (RATE / 2) as usize));
            return;
        }
    }
}

/// Feed PCM into the mic channel in 20 ms frames at real-time pace, then a
/// second of silence so voice-activity detection sees the utterance end.
async fn stream_as_mic(mic_tx: &mpsc::Sender<Vec<i16>>, samples: &[i16]) {
    let frame = (RATE / 50) as usize;
    for chunk in samples.chunks(frame) {
        if mic_tx.send(chunk.to_vec()).await.is_err() {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    for _ in 0..50 {
        if mic_tx.send(vec![0i16; frame]).await.is_err() {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
}

/// One caller utterance via the Gemini TTS API. Returns mono PCM16 at 24 kHz.
async fn tts(
    http: &reqwest::Client,
    api_key: &str,
    text: &str,
) -> Result<Vec<i16>, Box<dyn std::error::Error>> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/{TTS_MODEL}:generateContent?key={api_key}"
    );
    let body = serde_json::json!({
        "contents": [{ "parts": [{ "text": text }] }],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": { "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": CALLER_VOICE } } }
        }
    });
    let response: serde_json::Value = timeout(Duration::from_secs(60), async {
        http.post(&url).json(&body).send().await?.json().await
    })
    .await??;
    let b64 = response["candidates"][0]["content"]["parts"][0]["inlineData"]["data"]
        .as_str()
        .ok_or_else(|| format!("no audio in TTS response: {response}"))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64)?;
    Ok(bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect())
}

/// The flow's live verdict after a turn — which steps are done, which tools
/// the document currently admits.
fn print_flow_state(handle: &LiveHandle) {
    let done: Vec<String> = handle.state().get("flow:done").unwrap_or_default();
    if let Some(explanation) = handle.explain() {
        println!(
            "  [flow] done: [{}] · active: [{}] · admitted tools: [{}]",
            done.join(", "),
            explanation.active.join(", "),
            explanation.allowed_tools.join(", ")
        );
    }
}

fn write_wav(path: &str, samples: &[i16], rate: u32) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, out)
}
