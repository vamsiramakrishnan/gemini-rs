//! Twilio Media Streams — a phone call as two channels of JSON text frames.
//!
//! Twilio's [Media Streams](https://www.twilio.com/docs/voice/media-streams)
//! forks a live call's audio over a WebSocket: μ-law 8 kHz frames arrive as
//! base64 inside JSON text messages, and JSON text messages you send play
//! back to the caller. This module speaks that protocol and adapts it onto
//! [`voice::pump`](crate::voice::pump) — so a governed Live session answers
//! the phone with the same barge-in guarantees as a local microphone:
//!
//! - inbound `media` frames are μ-law-decoded to PCM16 @ 8 kHz and fed to the
//!   pump, which resamples to the session's 16 kHz input;
//! - session audio comes back from the pump at 8 kHz, is μ-law-encoded, and
//!   sent as outbound `media` frames;
//! - an interruption ([`Playback::Flush`](crate::voice::Playback)) becomes a
//!   Twilio `clear` message, dropping every frame Twilio has buffered — the
//!   telephone form of "stop talking the instant the caller does";
//! - DTMF digits land in session state under [`KEY_DTMF`] / [`KEY_DTMF_HISTORY`],
//!   where flow guards and watchers read them like any other fact.
//!
//! The bridge owns no socket. [`TwilioCall::attach`] returns a sender for the
//! text frames Twilio delivers and a receiver of the text frames to forward
//! back — wire them to any WebSocket server (see `examples/telephony` for an
//! axum one). Raw SIP (no Twilio in the path) is out of scope here; that is
//! the roadmap's `rsipstack` integration.

use std::collections::HashMap;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use gemini_adk_rs::live::LiveHandle;

use super::g711;
use crate::voice::{Playback, VoicePump, pump};

/// The sample rate of every Twilio Media Stream, both directions.
pub const TWILIO_HZ: u32 = 8_000;

// The state-key vocabulary is shared across every connector — see
// [`super::bridge`]. Re-exported here so existing imports keep working.
pub use super::bridge::{KEY_CALL_SID, KEY_DTMF, KEY_DTMF_HISTORY, KEY_STREAM_SID};

// ── Inbound protocol (Twilio → us) ───────────────────────────────────────────

/// Metadata delivered by Twilio's `start` frame when the stream opens.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct StartMeta {
    /// The stream SID — required in every frame sent back to Twilio.
    pub stream_sid: String,
    /// The SID of the underlying voice call.
    pub call_sid: String,
    /// The Twilio account SID.
    pub account_sid: String,
    /// Which tracks are being forked (normally `["inbound"]`).
    pub tracks: Vec<String>,
    /// Audio format of the stream (normally `audio/x-mulaw` @ 8000 Hz mono).
    pub media_format: MediaFormat,
    /// `<Parameter>` values set on the `<Stream>` TwiML noun.
    pub custom_parameters: HashMap<String, String>,
}

/// The audio encoding Twilio declares in the `start` frame.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaFormat {
    /// MIME-style encoding name, e.g. `audio/x-mulaw`.
    pub encoding: String,
    /// Sample rate in Hz (8000 for telephone audio).
    pub sample_rate: u32,
    /// Channel count (1 for a single forked track).
    pub channels: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaPayload {
    #[serde(default)]
    track: String,
    payload: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DtmfPayload {
    digit: String,
}

#[derive(Debug, Clone, Deserialize)]
struct MarkPayload {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
enum RawInbound {
    Connected {},
    Start { start: StartMeta },
    Media { media: MediaPayload },
    Dtmf { dtmf: DtmfPayload },
    Mark { mark: MarkPayload },
    Stop {},
}

/// One decoded frame from Twilio, ready for application handling.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    /// The WebSocket handshake completed (`connected`). No stream SID yet.
    Connected,
    /// The stream opened; carries SIDs, tracks, format, and TwiML parameters.
    Started(StartMeta),
    /// One chunk of caller audio, μ-law-decoded to mono PCM16 @ 8 kHz.
    Audio(Vec<i16>),
    /// The caller pressed a DTMF key.
    Dtmf(char),
    /// Twilio confirms playback reached a `mark` we sent.
    Mark(String),
    /// The stream ended (call hung up or `<Stream>` stopped).
    Stopped,
    /// An event this version does not model (forward-compatible skip).
    Ignored,
}

/// Parse one Twilio Media Streams text frame.
///
/// Unknown event types parse to [`Inbound::Ignored`] so a new Twilio event
/// never breaks an existing bridge; malformed JSON is an error.
pub fn parse_inbound(text: &str) -> Result<Inbound, TwilioError> {
    let raw: RawInbound = match serde_json::from_str(text) {
        Ok(raw) => raw,
        Err(_) => {
            // Distinguish "not JSON" from "JSON with an unknown event tag".
            let value: serde_json::Value =
                serde_json::from_str(text).map_err(TwilioError::Malformed)?;
            return if value.get("event").is_some() {
                Ok(Inbound::Ignored)
            } else {
                Err(TwilioError::NotAFrame)
            };
        }
    };
    Ok(match raw {
        RawInbound::Connected {} => Inbound::Connected,
        RawInbound::Start { start } => Inbound::Started(start),
        RawInbound::Media { media } => {
            // Only the caller's track feeds the session; if outbound audio is
            // also forked (`tracks: ["both"]`), skip our own voice.
            if !media.track.is_empty() && media.track != "inbound" {
                return Ok(Inbound::Ignored);
            }
            let mulaw = base64::engine::general_purpose::STANDARD
                .decode(media.payload.as_bytes())
                .map_err(TwilioError::BadPayload)?;
            Inbound::Audio(g711::decode_ulaw(&mulaw))
        }
        RawInbound::Dtmf { dtmf } => match dtmf.digit.chars().next() {
            Some(digit) => Inbound::Dtmf(digit),
            None => Inbound::Ignored,
        },
        RawInbound::Mark { mark } => Inbound::Mark(mark.name),
        RawInbound::Stop {} => Inbound::Stopped,
    })
}

/// Errors from parsing Twilio frames.
#[derive(Debug)]
pub enum TwilioError {
    /// The text was not valid JSON.
    Malformed(serde_json::Error),
    /// The JSON carried no `event` field — not a Media Streams frame.
    NotAFrame,
    /// A `media` frame's payload was not valid base64.
    BadPayload(base64::DecodeError),
}

impl std::fmt::Display for TwilioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "malformed Twilio frame: {e}"),
            Self::NotAFrame => write!(f, "JSON without an event field"),
            Self::BadPayload(e) => write!(f, "invalid base64 media payload: {e}"),
        }
    }
}

impl std::error::Error for TwilioError {}

// ── Outbound protocol (us → Twilio) ──────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutMedia<'a> {
    event: &'static str,
    stream_sid: &'a str,
    media: OutMediaPayload,
}

#[derive(Serialize)]
struct OutMediaPayload {
    payload: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutClear<'a> {
    event: &'static str,
    stream_sid: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutMark<'a> {
    event: &'static str,
    stream_sid: &'a str,
    mark: OutMarkPayload<'a>,
}

#[derive(Serialize)]
struct OutMarkPayload<'a> {
    name: &'a str,
}

/// Build an outbound `media` frame from mono PCM16 samples @ 8 kHz.
pub fn media_frame(stream_sid: &str, samples: &[i16]) -> String {
    let payload = base64::engine::general_purpose::STANDARD.encode(g711::encode_ulaw(samples));
    serde_json::to_string(&OutMedia {
        event: "media",
        stream_sid,
        media: OutMediaPayload { payload },
    })
    .expect("media frame serializes")
}

/// Build a `clear` frame — Twilio drops all buffered outbound audio.
/// This is barge-in on the telephone: send it on [`Playback::Flush`].
pub fn clear_frame(stream_sid: &str) -> String {
    serde_json::to_string(&OutClear {
        event: "clear",
        stream_sid,
    })
    .expect("clear frame serializes")
}

/// Build a `mark` frame — Twilio echoes it back once playback reaches it.
pub fn mark_frame(stream_sid: &str, name: &str) -> String {
    serde_json::to_string(&OutMark {
        event: "mark",
        stream_sid,
        mark: OutMarkPayload { name },
    })
    .expect("mark frame serializes")
}

// ── Call bridge ──────────────────────────────────────────────────────────────

/// 20 ms of audio at Twilio's 8 kHz: the frame size the network expects.
const FRAME_SAMPLES: usize = 160;
const FRAME_DURATION: std::time::Duration = std::time::Duration::from_millis(20);
/// How long to wait for more audio before padding a partial frame.
const TAIL_WAIT: std::time::Duration = std::time::Duration::from_millis(40);

/// State key set on barge-in: how many milliseconds of agent audio had been
/// sent but not yet played when the caller interrupted. The caller never
/// heard that part of the reply.
pub const KEY_UNPLAYED_MS: &str = "telephony:unplayed_ms";

/// Cuts agent audio into 20 ms frames and keeps the line's playout clock.
#[derive(Debug, Default)]
struct Framer {
    /// A partial frame waiting for the rest of its 20 ms.
    pending: Vec<i16>,
    /// When the audio sent so far finishes playing on the line.
    plays_until: Option<std::time::Instant>,
}

impl Framer {
    fn is_idle(&self) -> bool {
        self.pending.is_empty()
    }

    /// Queue audio; returns the whole frames now ready to send.
    fn push(&mut self, samples: &[i16], now: std::time::Instant) -> Vec<Vec<i16>> {
        self.pending.extend_from_slice(samples);
        let mut frames = Vec::new();
        while self.pending.len() >= FRAME_SAMPLES {
            frames.push(self.pending.drain(..FRAME_SAMPLES).collect());
            let from = self.plays_until.map_or(now, |t| t.max(now));
            self.plays_until = Some(from + FRAME_DURATION);
        }
        frames
    }

    /// Pad the partial frame with silence, so the next `push` sends it.
    fn pad_tail(&mut self) {
        if !self.pending.is_empty() {
            self.pending.resize(FRAME_SAMPLES, 0);
        }
    }

    /// Barge-in: drop what is queued; returns how much sent audio had not
    /// played yet.
    fn flush(&mut self, now: std::time::Instant) -> std::time::Duration {
        self.pending.clear();
        let unplayed = self.plays_until.map_or(std::time::Duration::ZERO, |t| {
            t.saturating_duration_since(now)
        });
        self.plays_until = Some(now);
        unplayed
    }
}

/// Options for [`TwilioCall::attach_with`].
#[derive(Clone, Default)]
pub struct CallOptions {
    /// Record the call in stereo (caller left, agent right), as heard.
    pub recorder: Option<std::sync::Arc<super::recorder::CallRecorder>>,
    /// Also detect keypresses in the audio. Twilio reports keypresses as
    /// `dtmf` events, so this is off by default to avoid counting twice.
    pub in_band_dtmf: bool,
}

/// A live phone call attached to a session — the telephone counterpart of
/// `Talk::talk`.
///
/// [`attach`](TwilioCall::attach) wires the session's [`pump`] to a pair of
/// text-frame channels speaking the Media Streams protocol. The caller owns
/// the WebSocket: forward every text message Twilio delivers into
/// [`from_twilio`](TwilioCall::from_twilio), and forward every message from
/// [`to_twilio`](TwilioCall::to_twilio) back over the socket.
///
/// ```ignore
/// // `ignore`: `ws` is the application's Twilio WebSocket (see examples/telephony).
/// let session = Live::builder()
///     .instruction("You are the front desk. Answer the call.")
///     .greeting("Greet the caller.")
///     .connect_from_env().await?;
/// let mut call = TwilioCall::attach(&session);
/// loop {
///     tokio::select! {
///         Some(msg) = ws.recv() => call.from_twilio.send(msg.into_text()?).await?,
///         Some(out) = call.to_twilio.recv() => ws.send(Message::Text(out)).await?,
///         else => break,
///     }
/// }
/// ```
pub struct TwilioCall {
    /// Feed Twilio's inbound WebSocket text frames here.
    pub from_twilio: mpsc::Sender<String>,
    /// Text frames to send back to Twilio over the WebSocket.
    pub to_twilio: mpsc::Receiver<String>,
    pump: VoicePump,
    inbound_task: JoinHandle<()>,
    outbound_task: JoinHandle<()>,
}

impl TwilioCall {
    /// Attach a Twilio Media Stream to a connected session, with the
    /// default [`CallOptions`].
    ///
    /// Audio starts flowing once Twilio's `start` frame arrives (outbound
    /// frames need its stream SID; audio generated before it is dropped —
    /// there is no call to play it into yet). Stream metadata and DTMF
    /// digits are written into session state under the `telephony:` keys.
    pub fn attach(handle: &LiveHandle) -> TwilioCall {
        Self::attach_with(handle, CallOptions::default())
    }

    /// Attach a Twilio Media Stream to a connected session.
    ///
    /// Beyond [`attach`](Self::attach):
    /// - the caller's audio passes through a
    ///   [`KeypadGuard`](super::bridge::KeypadGuard), so keypad tones never
    ///   reach the model and a masked keypad silences the caller;
    /// - the agent's audio goes out in 20 ms frames, as the network expects,
    ///   with the last partial frame padded once the model pauses;
    /// - on barge-in, [`KEY_UNPLAYED_MS`] records how much queued agent
    ///   audio the caller never heard;
    /// - with [`CallOptions::recorder`], the call is recorded in stereo as
    ///   heard.
    pub fn attach_with(handle: &LiveHandle, options: CallOptions) -> TwilioCall {
        let (from_tx, mut from_rx) = mpsc::channel::<String>(64);
        let (to_tx, to_rx) = mpsc::channel::<String>(64);
        let (mic_tx, mic_rx) = mpsc::channel::<Vec<i16>>(64);
        let (speaker_tx, mut speaker_rx) = mpsc::channel::<Playback>(64);
        let (sid_tx, sid_rx) = watch::channel::<Option<String>>(None);

        let voice_pump = pump(handle, mic_rx, TWILIO_HZ, speaker_tx, TWILIO_HZ);

        let state = handle.state().clone();
        let mut guard = super::bridge::KeypadGuard::new(state.clone(), TWILIO_HZ)
            .record_in_band(options.in_band_dtmf);
        let inbound_recorder = options.recorder.clone();
        let inbound_task = tokio::spawn(async move {
            use crate::voice::InputAudioProcessor as _;
            while let Some(text) = from_rx.recv().await {
                match parse_inbound(&text) {
                    Ok(Inbound::Audio(mut samples)) => {
                        guard.process_frame(&mut samples);
                        if let Some(recorder) = &inbound_recorder {
                            recorder.caller(&samples);
                        }
                        if mic_tx.send(samples).await.is_err() {
                            break;
                        }
                    }
                    Ok(Inbound::Started(meta)) => {
                        let _ = state.set(KEY_CALL_SID, meta.call_sid.clone());
                        let _ = state.set(KEY_STREAM_SID, meta.stream_sid.clone());
                        let _ = sid_tx.send(Some(meta.stream_sid));
                    }
                    Ok(Inbound::Dtmf(digit)) => super::bridge::record_dtmf(&state, digit),
                    Ok(Inbound::Stopped) => break,
                    Ok(Inbound::Connected | Inbound::Mark(_) | Inbound::Ignored) => {}
                    Err(err) => tracing::warn!("dropping unparseable Twilio frame: {err}"),
                }
            }
        });

        let out_state = handle.state().clone();
        let outbound_recorder = options.recorder;
        let outbound_task = tokio::spawn(async move {
            let mut framer = Framer::default();
            loop {
                let next = if framer.is_idle() {
                    speaker_rx.recv().await
                } else {
                    match tokio::time::timeout(TAIL_WAIT, speaker_rx.recv()).await {
                        Ok(next) => next,
                        // The model paused mid-frame: pad and send the tail.
                        Err(_) => {
                            framer.pad_tail();
                            Some(Playback::Chunk(Vec::new()))
                        }
                    }
                };
                let Some(playback) = next else { break };
                // Frames are unsendable until `start` delivers the SID.
                let Some(sid) = sid_rx.borrow().clone() else {
                    continue;
                };
                let now = std::time::Instant::now();
                let mut frames = Vec::new();
                match playback {
                    Playback::Chunk(samples) => {
                        for frame in framer.push(&samples, now) {
                            if let Some(recorder) = &outbound_recorder {
                                recorder.agent(&frame);
                            }
                            frames.push(media_frame(&sid, &frame));
                        }
                    }
                    Playback::Flush => {
                        let unplayed = framer.flush(now);
                        let _ = out_state.set(KEY_UNPLAYED_MS, unplayed.as_millis() as u64);
                        if let Some(recorder) = &outbound_recorder {
                            recorder.flush();
                        }
                        frames.push(clear_frame(&sid));
                    }
                }
                for frame in frames {
                    if to_tx.send(frame).await.is_err() {
                        return;
                    }
                }
            }
        });

        TwilioCall {
            from_twilio: from_tx,
            to_twilio: to_rx,
            pump: voice_pump,
            inbound_task,
            outbound_task,
        }
    }

    /// Wait until the call ends (stream stopped, session closed, or the
    /// WebSocket side dropped the channels).
    pub async fn join(self) {
        let _ = self.inbound_task.await;
        let _ = self.outbound_task.await;
        self.pump.join().await;
    }

    /// Tear the bridge down immediately.
    pub fn abort(&self) {
        self.inbound_task.abort();
        self.outbound_task.abort();
        self.pump.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_audio_goes_out_in_20ms_frames_on_a_playout_clock() {
        let t0 = std::time::Instant::now();
        let mut framer = Framer::default();
        // 50 ms of audio in one burst: two whole frames now, 10 ms pending.
        let frames = framer.push(&[1; 400], t0);
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|f| f.len() == FRAME_SAMPLES));
        assert!(!framer.is_idle());
        // The model pauses: the tail is padded to a full frame.
        framer.pad_tail();
        let tail = framer.push(&[], t0);
        assert_eq!(tail.len(), 1);
        assert_eq!(&tail[0][..80], &[1; 80]);
        assert_eq!(&tail[0][80..], &[0; 80]);
        // 60 ms were sent at t0; a barge-in 25 ms later cuts 35 ms.
        let unplayed = framer.flush(t0 + std::time::Duration::from_millis(25));
        assert_eq!(unplayed, std::time::Duration::from_millis(35));
        assert!(framer.is_idle());
    }

    #[test]
    fn parses_the_start_frame() {
        let text = r#"{
            "event": "start", "sequenceNumber": "1", "streamSid": "MZxyz",
            "start": {
                "accountSid": "ACabc", "streamSid": "MZxyz", "callSid": "CAdef",
                "tracks": ["inbound"],
                "mediaFormat": {"encoding": "audio/x-mulaw", "sampleRate": 8000, "channels": 1},
                "customParameters": {"agent": "front-desk"}
            }
        }"#;
        match parse_inbound(text).unwrap() {
            Inbound::Started(meta) => {
                assert_eq!(meta.stream_sid, "MZxyz");
                assert_eq!(meta.call_sid, "CAdef");
                assert_eq!(meta.media_format.sample_rate, 8000);
                assert_eq!(meta.custom_parameters["agent"], "front-desk");
            }
            other => panic!("expected Started, got {other:?}"),
        }
    }

    #[test]
    fn media_frames_decode_to_pcm() {
        // 0xFF is μ-law silence; four bytes → four zero samples.
        let payload = base64::engine::general_purpose::STANDARD.encode([0xFFu8; 4]);
        let text = format!(
            r#"{{"event":"media","streamSid":"MZ1","media":{{"track":"inbound","chunk":"1","timestamp":"5","payload":"{payload}"}}}}"#
        );
        assert_eq!(parse_inbound(&text).unwrap(), Inbound::Audio(vec![0i16; 4]));
    }

    #[test]
    fn outbound_track_media_is_skipped() {
        let payload = base64::engine::general_purpose::STANDARD.encode([0xFFu8; 4]);
        let text = format!(
            r#"{{"event":"media","streamSid":"MZ1","media":{{"track":"outbound","payload":"{payload}"}}}}"#
        );
        assert_eq!(parse_inbound(&text).unwrap(), Inbound::Ignored);
    }

    #[test]
    fn dtmf_marks_stop_and_unknown_events() {
        assert_eq!(
            parse_inbound(
                r#"{"event":"dtmf","streamSid":"MZ1","dtmf":{"track":"inbound_track","digit":"7"}}"#
            )
            .unwrap(),
            Inbound::Dtmf('7')
        );
        assert_eq!(
            parse_inbound(r#"{"event":"mark","streamSid":"MZ1","mark":{"name":"m1"}}"#).unwrap(),
            Inbound::Mark("m1".into())
        );
        assert_eq!(
            parse_inbound(r#"{"event":"stop","streamSid":"MZ1","stop":{}}"#).unwrap(),
            Inbound::Stopped
        );
        assert_eq!(
            parse_inbound(r#"{"event":"connected","protocol":"Call","version":"1.0.0"}"#).unwrap(),
            Inbound::Connected
        );
        // Forward compatibility: an event we don't model is skipped, not fatal.
        assert_eq!(
            parse_inbound(r#"{"event":"totally-new-thing"}"#).unwrap(),
            Inbound::Ignored
        );
        assert!(parse_inbound("not json").is_err());
        assert!(parse_inbound(r#"{"no_event": true}"#).is_err());
    }

    #[test]
    fn media_frame_round_trips_through_the_codec() {
        let samples = vec![0i16, 1000, -1000, 8000];
        let frame = media_frame("MZ1", &samples);
        let value: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(value["event"], "media");
        assert_eq!(value["streamSid"], "MZ1");
        let mulaw = base64::engine::general_purpose::STANDARD
            .decode(value["media"]["payload"].as_str().unwrap())
            .unwrap();
        let decoded = g711::decode_ulaw(&mulaw);
        assert_eq!(decoded.len(), samples.len());
        // Companded round-trip: close, not exact.
        for (orig, rt) in samples.iter().zip(&decoded) {
            assert!(((orig - rt) as i32).abs() <= (orig.unsigned_abs() as i32 / 16).max(16));
        }
    }

    #[test]
    fn clear_and_mark_frames_have_the_wire_shape() {
        let clear: serde_json::Value = serde_json::from_str(&clear_frame("MZ9")).unwrap();
        assert_eq!(
            clear,
            serde_json::json!({"event": "clear", "streamSid": "MZ9"})
        );
        let mark: serde_json::Value = serde_json::from_str(&mark_frame("MZ9", "done")).unwrap();
        assert_eq!(
            mark,
            serde_json::json!({"event": "mark", "streamSid": "MZ9", "mark": {"name": "done"}})
        );
    }
}
