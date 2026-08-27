//! The AudioHook wire protocol, as a pure state machine.
//!
//! [AudioHook](https://developer.genesys.cloud/devapps/audiohook/) is the
//! open WebSocket protocol a Genesys-style contact-center platform uses to
//! stream a live call to an external voice bot: JSON text frames carry the
//! session lifecycle (`open`/`opened`, `ping`/`pong`, `dtmf`,
//! `close`/`closed`), binary frames carry raw μ-law audio at 8 kHz — both
//! directions. The platform is the WebSocket *client*; this server is the
//! bot end.
//!
//! [`ServerSession`] owns everything the wire dialect requires — envelope
//! sequencing (`seq`/`clientseq`), media negotiation, position tracking,
//! μ-law decode, connection-probe detection — and none of what it doesn't:
//! no socket, no session, no channels. Feed it frames, act on the
//! [`Effect`]s it returns. That split is what makes the protocol testable
//! offline (see the tests below) and the glue in `main.rs` small.

use serde::Deserialize;
use serde_json::{json, Value};

use gemini_adk_fluent_rs::telephony::g711;

/// The only audio format this bot end negotiates: μ-law at the telephone
/// rate. Everything after the handshake is PCM16 at this rate.
pub const AUDIOHOOK_HZ: u32 = 8_000;

/// The conversation id a platform sends on a *connection probe* — a
/// handshake-only health check made when the integration is configured.
/// A probe must complete `open`/`opened` but starts no real call.
pub const PROBE_CONVERSATION_ID: &str = "00000000-0000-0000-0000-000000000000";

// ── Client → server messages ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct ClientMessage {
    /// Message type tag. Matched as a string so an unmodeled type is
    /// skipped, never fatal.
    #[serde(rename = "type")]
    kind: String,
    /// The client's own message counter; echoed back as `clientseq`.
    #[serde(default)]
    seq: u64,
    /// Session id, assigned by the platform in `open` and constant after.
    #[serde(default)]
    id: String,
    #[serde(default)]
    parameters: Value,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct OpenParameters {
    conversation_id: String,
    participant: Participant,
    media: Vec<MediaEntry>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Participant {
    /// The caller's number (automatic number identification).
    ani: String,
    /// The number the caller dialed.
    dnis: String,
}

#[derive(Deserialize, Clone, Default, PartialEq)]
#[serde(default)]
struct MediaEntry {
    #[serde(rename = "type")]
    kind: String,
    format: String,
    channels: Vec<String>,
    rate: u32,
}

/// What an `open` message told us about the call.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenInfo {
    /// The platform's session id (the `id` field of every message).
    pub session_id: String,
    /// The platform's conversation id — the call, across transfers.
    pub conversation_id: String,
    /// Caller's number, when the platform shares it.
    pub ani: String,
    /// Dialed number.
    pub dnis: String,
    /// `true` for a handshake-only connection probe: complete the
    /// handshake, start no session, wait for `close`.
    pub probe: bool,
}

/// What the state machine wants done after a frame. Order matters:
/// replies must reach the wire in the order returned.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// A JSON text frame to send to the platform.
    Reply(String),
    /// The caller pressed a DTMF key.
    Dtmf(char),
    /// The `open` handshake completed (the `opened` reply is already in a
    /// preceding [`Effect::Reply`]). Time to start — or skip — a session.
    Opened(OpenInfo),
    /// The conversation is over (`close` answered with `closed`, or media
    /// negotiation failed): drop the connection after flushing replies.
    End,
}

/// Errors that end the connection: a text frame that is not AudioHook.
#[derive(Debug)]
pub enum ProtocolError {
    /// The text frame was not valid JSON or carried no `type`.
    Malformed(serde_json::Error),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "malformed AudioHook frame: {e}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

// ── The server end ───────────────────────────────────────────────────────────

/// The bot-server end of one AudioHook connection.
///
/// Owns the protocol invariants and nothing else: every outgoing message is
/// built here so `seq` increments exactly once per message, `clientseq`
/// always names the last client message seen, and `position` reflects the
/// audio actually received.
pub struct ServerSession {
    /// Our outgoing message counter.
    seq: u64,
    /// The last client `seq` seen; echoed in every reply.
    client_seq: u64,
    /// Session id from `open`; empty until then.
    session_id: String,
    /// Negotiated stream is stereo (external + internal interleaved).
    stereo: bool,
    /// Handshake state: audio and DTMF are only meaningful in between.
    opened: bool,
    closed: bool,
    /// Caller samples received, for the `position` field.
    samples_received: u64,
}

impl Default for ServerSession {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerSession {
    /// A fresh connection, before `open`.
    pub fn new() -> Self {
        Self {
            seq: 0,
            client_seq: 0,
            session_id: String::new(),
            stereo: false,
            opened: false,
            closed: false,
            samples_received: 0,
        }
    }

    /// Handle one text frame from the platform.
    pub fn handle_text(&mut self, text: &str) -> Result<Vec<Effect>, ProtocolError> {
        let message: ClientMessage =
            serde_json::from_str(text).map_err(ProtocolError::Malformed)?;
        self.client_seq = message.seq;
        Ok(match message.kind.as_str() {
            "open" => self.handle_open(message),
            "ping" => vec![Effect::Reply(self.message("pong", json!({})))],
            "close" => {
                self.closed = true;
                vec![
                    Effect::Reply(self.message("closed", json!({}))),
                    Effect::End,
                ]
            }
            "dtmf" => match message.parameters["digit"]
                .as_str()
                .and_then(|digit| digit.chars().next())
            {
                Some(digit) if self.opened && !self.closed => vec![Effect::Dtmf(digit)],
                _ => vec![],
            },
            "error" => {
                tracing::warn!(
                    "platform reported error: {}",
                    message.parameters.to_string()
                );
                vec![]
            }
            // paused/resumed/update/discarded and anything newer: valid
            // protocol we don't act on. Skipping keeps us forward-compatible.
            _ => vec![],
        })
    }

    /// Handle one binary frame: caller audio in the negotiated format.
    /// Frames before `open` completes (or after `close`) are dropped.
    pub fn handle_binary(&mut self, payload: &[u8]) -> Option<Vec<i16>> {
        if !self.opened || self.closed {
            return None;
        }
        // μ-law is one byte per sample; a stereo stream interleaves
        // external (caller) and internal (agent) channels byte by byte.
        // Only the caller's channel feeds the session.
        let external: Vec<u8> = if self.stereo {
            payload.iter().copied().step_by(2).collect()
        } else {
            payload.to_vec()
        };
        self.samples_received += external.len() as u64;
        Some(g711::decode_ulaw(&external))
    }

    fn handle_open(&mut self, message: ClientMessage) -> Vec<Effect> {
        self.session_id = message.id;
        let parameters: OpenParameters =
            serde_json::from_value(message.parameters).unwrap_or_default();

        // Media negotiation: μ-law @ 8 kHz, caller channel only if offered
        // that way, otherwise the stereo form (we de-interleave). Anything
        // else on offer is not something this bot speaks.
        let usable = |entry: &&MediaEntry| {
            entry.kind == "audio" && entry.format == "PCMU" && entry.rate == AUDIOHOOK_HZ
        };
        let mono = parameters
            .media
            .iter()
            .filter(usable)
            .find(|entry| entry.channels == ["external"]);
        let selected = mono.or_else(|| {
            parameters
                .media
                .iter()
                .filter(usable)
                .find(|entry| entry.channels.iter().any(|channel| channel == "external"))
        });

        let Some(selected) = selected.cloned() else {
            return vec![
                Effect::Reply(self.message(
                    "disconnect",
                    json!({"reason": "error", "info": "no supported media offered (need PCMU 8000 with an external channel)"}),
                )),
                Effect::End,
            ];
        };

        self.stereo = selected.channels.len() > 1;
        self.opened = true;
        let opened = self.message(
            "opened",
            json!({
                "startPaused": false,
                "media": [{
                    "type": "audio",
                    "format": "PCMU",
                    "channels": selected.channels,
                    "rate": AUDIOHOOK_HZ,
                }],
            }),
        );
        vec![
            Effect::Reply(opened),
            Effect::Opened(OpenInfo {
                session_id: self.session_id.clone(),
                conversation_id: parameters.conversation_id.clone(),
                ani: parameters.participant.ani,
                dnis: parameters.participant.dnis,
                probe: parameters.conversation_id == PROBE_CONVERSATION_ID,
            }),
        ]
    }

    /// Barge-in: tell the platform to drop every frame of bot audio it has
    /// buffered. Send on [`Playback::Flush`](gemini_adk_fluent_rs::voice::Playback)
    /// — the AudioHook form of Twilio's `clear`.
    pub fn barge_in_event(&mut self) -> String {
        self.message(
            "event",
            json!({"entities": [{"type": "barge_in", "data": {}}]}),
        )
    }

    /// Server-initiated end of conversation. `reason` is `"completed"` for
    /// a normal end; a warm transfer puts the handoff packet in
    /// `output_variables`, where the platform's flow can route on it.
    pub fn disconnect(&mut self, reason: &str, output_variables: Value) -> String {
        self.message(
            "disconnect",
            json!({"reason": reason, "outputVariables": output_variables}),
        )
    }

    /// Whether the `open` handshake has completed (and `close` has not).
    pub fn is_open(&self) -> bool {
        self.opened && !self.closed
    }

    /// Build one outgoing message with the envelope every AudioHook message
    /// carries. The single place `seq` increments.
    fn message(&mut self, kind: &str, parameters: Value) -> String {
        self.seq += 1;
        json!({
            "version": "2",
            "type": kind,
            "seq": self.seq,
            "clientseq": self.client_seq,
            "id": self.session_id,
            "position": self.position(),
            "parameters": parameters,
        })
        .to_string()
    }

    /// Stream position as an ISO 8601 duration of caller audio received.
    fn position(&self) -> String {
        format!(
            "PT{:.3}S",
            self.samples_received as f64 / f64::from(AUDIOHOOK_HZ)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_frame(conversation_id: &str, media: Value) -> String {
        json!({
            "version": "2", "type": "open", "seq": 1, "serverseq": 0,
            "id": "e160e428-53b2-487c-8158-29283bd5ba2a", "position": "PT0S",
            "parameters": {
                "organizationId": "d7934305-0972-4844-938e-9060eef73d05",
                "conversationId": conversation_id,
                "participant": {"id": "883efee8", "ani": "+14805551234", "dnis": "+18005559876"},
                "media": media,
            },
        })
        .to_string()
    }

    fn mono_pcmu() -> Value {
        json!([{"type": "audio", "format": "PCMU", "channels": ["external"], "rate": 8000}])
    }

    fn parse(reply: &Effect) -> Value {
        match reply {
            Effect::Reply(text) => serde_json::from_str(text).unwrap(),
            other => panic!("expected Reply, got {other:?}"),
        }
    }

    #[test]
    fn open_negotiates_mono_pcmu_and_reports_the_call() {
        let mut session = ServerSession::new();
        let effects = session
            .handle_text(&open_frame("09b7...conv", mono_pcmu()))
            .unwrap();
        assert_eq!(effects.len(), 2);

        let opened = parse(&effects[0]);
        assert_eq!(opened["type"], "opened");
        assert_eq!(opened["version"], "2");
        assert_eq!(opened["seq"], 1);
        assert_eq!(opened["clientseq"], 1);
        assert_eq!(opened["id"], "e160e428-53b2-487c-8158-29283bd5ba2a");
        assert_eq!(opened["parameters"]["media"][0]["format"], "PCMU");
        assert_eq!(
            opened["parameters"]["media"][0]["channels"],
            json!(["external"])
        );
        assert_eq!(opened["parameters"]["startPaused"], false);

        match &effects[1] {
            Effect::Opened(info) => {
                assert_eq!(info.ani, "+14805551234");
                assert_eq!(info.dnis, "+18005559876");
                assert_eq!(info.conversation_id, "09b7...conv");
                assert!(!info.probe);
            }
            other => panic!("expected Opened, got {other:?}"),
        }
        assert!(session.is_open());
    }

    #[test]
    fn stereo_only_offer_is_accepted_and_deinterleaved() {
        let mut session = ServerSession::new();
        let media = json!([
            {"type": "audio", "format": "PCMU", "channels": ["external", "internal"], "rate": 8000},
        ]);
        let effects = session.handle_text(&open_frame("conv", media)).unwrap();
        let opened = parse(&effects[0]);
        assert_eq!(
            opened["parameters"]["media"][0]["channels"],
            json!(["external", "internal"])
        );
        // external is the even byte of each interleaved pair; 0xFF is μ-law
        // silence (decodes to 0), 0x7F decodes to a large negative value.
        let pcm = session.handle_binary(&[0xFF, 0x7F, 0xFF, 0x7F]).unwrap();
        assert_eq!(pcm, vec![0i16, 0]);
    }

    #[test]
    fn unsupported_media_draws_a_disconnect() {
        let mut session = ServerSession::new();
        let media = json!([
            {"type": "audio", "format": "L16", "channels": ["external"], "rate": 16000},
        ]);
        let effects = session.handle_text(&open_frame("conv", media)).unwrap();
        let disconnect = parse(&effects[0]);
        assert_eq!(disconnect["type"], "disconnect");
        assert_eq!(disconnect["parameters"]["reason"], "error");
        assert_eq!(effects[1], Effect::End);
        assert!(!session.is_open());
    }

    #[test]
    fn connection_probe_is_flagged_but_still_handshakes() {
        let mut session = ServerSession::new();
        let effects = session
            .handle_text(&open_frame(PROBE_CONVERSATION_ID, mono_pcmu()))
            .unwrap();
        assert_eq!(parse(&effects[0])["type"], "opened");
        match &effects[1] {
            Effect::Opened(info) => assert!(info.probe),
            other => panic!("expected Opened, got {other:?}"),
        }
    }

    #[test]
    fn ping_pong_keeps_the_envelope_sequenced() {
        let mut session = ServerSession::new();
        session
            .handle_text(&open_frame("conv", mono_pcmu()))
            .unwrap();
        let ping = json!({
            "version": "2", "type": "ping", "seq": 2, "serverseq": 1,
            "id": "e160e428-53b2-487c-8158-29283bd5ba2a", "position": "PT1S",
            "parameters": {},
        });
        let effects = session.handle_text(&ping.to_string()).unwrap();
        let pong = parse(&effects[0]);
        assert_eq!(pong["type"], "pong");
        assert_eq!(pong["seq"], 2, "our second message");
        assert_eq!(pong["clientseq"], 2, "echoes the ping's seq");
    }

    #[test]
    fn caller_audio_decodes_and_advances_position() {
        let mut session = ServerSession::new();
        // Audio before open is dropped, and does not advance position.
        assert_eq!(session.handle_binary(&[0xFF; 8]), None);
        session
            .handle_text(&open_frame("conv", mono_pcmu()))
            .unwrap();

        let pcm = session.handle_binary(&[0xFF; 8000]).unwrap();
        assert_eq!(pcm.len(), 8000);
        assert!(pcm.iter().all(|&sample| sample == 0));

        // One second of audio received → position PT1.000S on the next reply.
        let ping = json!({"version": "2", "type": "ping", "seq": 2, "id": "x", "parameters": {}});
        let effects = session.handle_text(&ping.to_string()).unwrap();
        assert_eq!(parse(&effects[0])["position"], "PT1.000S");
    }

    #[test]
    fn dtmf_lands_only_while_open() {
        let mut session = ServerSession::new();
        let dtmf = json!({"version": "2", "type": "dtmf", "seq": 2, "id": "x",
            "parameters": {"digit": "5"}})
        .to_string();
        assert_eq!(session.handle_text(&dtmf).unwrap(), vec![]);
        session
            .handle_text(&open_frame("conv", mono_pcmu()))
            .unwrap();
        assert_eq!(session.handle_text(&dtmf).unwrap(), vec![Effect::Dtmf('5')]);
    }

    #[test]
    fn close_is_answered_with_closed_then_ends() {
        let mut session = ServerSession::new();
        session
            .handle_text(&open_frame("conv", mono_pcmu()))
            .unwrap();
        let close = json!({"version": "2", "type": "close", "seq": 3, "id": "x",
            "parameters": {"reason": "end"}})
        .to_string();
        let effects = session.handle_text(&close).unwrap();
        assert_eq!(parse(&effects[0])["type"], "closed");
        assert_eq!(effects[1], Effect::End);
        // After close: audio is dropped, the session is not open.
        assert_eq!(session.handle_binary(&[0xFF; 4]), None);
        assert!(!session.is_open());
    }

    #[test]
    fn barge_in_and_disconnect_have_the_wire_shape() {
        let mut session = ServerSession::new();
        session
            .handle_text(&open_frame("conv", mono_pcmu()))
            .unwrap();

        let event: Value = serde_json::from_str(&session.barge_in_event()).unwrap();
        assert_eq!(event["type"], "event");
        assert_eq!(event["parameters"]["entities"][0]["type"], "barge_in");

        let bye: Value =
            serde_json::from_str(&session.disconnect("completed", json!({"resolved": true})))
                .unwrap();
        assert_eq!(bye["type"], "disconnect");
        assert_eq!(bye["parameters"]["reason"], "completed");
        assert_eq!(bye["parameters"]["outputVariables"]["resolved"], true);
        // seq advanced once per message: opened, event, disconnect.
        assert_eq!(bye["seq"], 3);
    }

    #[test]
    fn unknown_types_are_skipped_not_fatal() {
        let mut session = ServerSession::new();
        let unknown = json!({"version": "2", "type": "paused", "seq": 4, "id": "x",
            "parameters": {}})
        .to_string();
        assert_eq!(session.handle_text(&unknown).unwrap(), vec![]);
        assert!(session.handle_text("not json").is_err());
    }
}
