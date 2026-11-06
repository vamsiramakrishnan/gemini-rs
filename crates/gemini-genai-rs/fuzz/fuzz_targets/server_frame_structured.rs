//! Structured server frames through the Live frame decoder.
//!
//! The fuzzer's bytes are turned into a well-formed server message (setup,
//! content with audio, text and transcription parts, tool calls, GoAway,
//! resumption, VAD) so coverage reaches the parser's branches rather than
//! stopping at JSON syntax. The decoder must never panic, must classify the
//! frame by its top-level key, and must hand audio bytes through unchanged.
#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use base64::Engine as _;
use gemini_genai_rs::protocol::messages::ServerMessage;
use gemini_genai_rs::transport::{Codec, JsonCodec};
use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};

#[derive(Arbitrary, Debug)]
enum Part {
    Text(String),
    Audio { data: Vec<u8>, mime: Mime },
    Thought(String),
    Executable(String),
}

#[derive(Arbitrary, Debug)]
enum Mime {
    Pcm,
    PcmRate,
    Video,
    Arbitrary(String),
}

#[derive(Arbitrary, Debug)]
enum Frame {
    SetupComplete,
    Content {
        parts: Vec<Part>,
        turn_complete: Option<bool>,
        generation_complete: Option<bool>,
        interrupted: Option<bool>,
        input_transcription: Option<String>,
        output_transcription: Option<String>,
        turn_complete_reason: Option<String>,
        usage: Option<(u32, u32, u32)>,
    },
    ToolCall(Vec<(String, Value2, Option<String>)>),
    ToolCallCancellation(Vec<String>),
    GoAway(Option<String>),
    SessionResumption {
        handle: Option<String>,
        resumable: Option<bool>,
        index: Option<Index>,
    },
    VoiceActivity {
        kind: Option<Activity>,
        offset: Option<String>,
    },
    UsageOnly(u32),
    Unknown(String, Value2),
    Nested(Vec<u8>),
}

#[derive(Arbitrary, Debug)]
enum Index {
    Number(u64),
    Text(String),
    Bool(bool),
}

#[derive(Arbitrary, Debug)]
enum Activity {
    Start,
    End,
    LegacyStart,
    Unknown(String),
}

/// A small JSON value grammar: enough to make tool arguments and unknown
/// payloads vary in shape without generating unbounded trees.
#[derive(Arbitrary, Debug)]
enum Value2 {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value2>),
    Map(Vec<(String, Value2)>),
}

impl Value2 {
    fn to_json(&self, depth: u8) -> Value {
        match self {
            Value2::Null => Value::Null,
            Value2::Bool(b) => json!(b),
            Value2::Int(i) => json!(i),
            Value2::Float(f) if f.is_finite() => json!(f),
            Value2::Float(_) => Value::Null,
            Value2::Str(s) => json!(s),
            Value2::List(items) if depth < 4 => {
                Value::Array(items.iter().take(8).map(|v| v.to_json(depth + 1)).collect())
            }
            Value2::Map(items) if depth < 4 => Value::Object(
                items
                    .iter()
                    .take(8)
                    .map(|(k, v)| (k.clone(), v.to_json(depth + 1)))
                    .collect(),
            ),
            Value2::List(_) | Value2::Map(_) => Value::Null,
        }
    }
}

fn part_json(part: &Part) -> Value {
    match part {
        Part::Text(text) => json!({ "text": text }),
        Part::Audio { data, mime } => {
            let mime = match mime {
                Mime::Pcm => "audio/pcm".to_string(),
                Mime::PcmRate => "audio/pcm;rate=24000".to_string(),
                Mime::Video => "video/mp4".to_string(),
                Mime::Arbitrary(s) => s.clone(),
            };
            json!({ "inlineData": { "mimeType": mime, "data": base64::engine::general_purpose::STANDARD.encode(data) } })
        }
        Part::Thought(text) => json!({ "text": text, "thought": true }),
        Part::Executable(code) => json!({ "executableCode": { "language": "PYTHON", "code": code } }),
    }
}

fn frame_json(frame: &Frame) -> Value {
    match frame {
        Frame::SetupComplete => json!({ "setupComplete": {} }),
        Frame::Content {
            parts,
            turn_complete,
            generation_complete,
            interrupted,
            input_transcription,
            output_transcription,
            turn_complete_reason,
            usage,
        } => {
            let mut content = serde_json::Map::new();
            if !parts.is_empty() {
                content.insert(
                    "modelTurn".into(),
                    json!({ "role": "model", "parts": parts.iter().take(16).map(part_json).collect::<Vec<_>>() }),
                );
            }
            if let Some(v) = turn_complete {
                content.insert("turnComplete".into(), json!(v));
            }
            if let Some(v) = generation_complete {
                content.insert("generationComplete".into(), json!(v));
            }
            if let Some(v) = interrupted {
                content.insert("interrupted".into(), json!(v));
            }
            if let Some(t) = input_transcription {
                content.insert("inputTranscription".into(), json!({ "text": t }));
            }
            if let Some(t) = output_transcription {
                content.insert("outputTranscription".into(), json!({ "text": t }));
            }
            if let Some(r) = turn_complete_reason {
                content.insert("turnCompleteReason".into(), json!(r));
            }
            let mut frame = json!({ "serverContent": Value::Object(content) });
            if let Some((p, c, t)) = usage {
                frame["usageMetadata"] = json!({ "promptTokenCount": p, "candidatesTokenCount": c, "totalTokenCount": t });
            }
            frame
        }
        Frame::ToolCall(calls) => json!({
            "toolCall": { "functionCalls": calls.iter().take(16).map(|(name, args, id)| {
                let mut call = json!({ "name": name, "args": args.to_json(0) });
                if let Some(id) = id { call["id"] = json!(id); }
                call
            }).collect::<Vec<_>>() }
        }),
        Frame::ToolCallCancellation(ids) => json!({ "toolCallCancellation": { "ids": ids } }),
        Frame::GoAway(time_left) => match time_left {
            Some(t) => json!({ "goAway": { "timeLeft": t } }),
            None => json!({ "goAway": {} }),
        },
        Frame::SessionResumption { handle, resumable, index } => {
            let mut payload = serde_json::Map::new();
            if let Some(h) = handle {
                payload.insert("newHandle".into(), json!(h));
            }
            if let Some(r) = resumable {
                payload.insert("resumable".into(), json!(r));
            }
            match index {
                Some(Index::Number(n)) => {
                    payload.insert("lastConsumedClientMessageIndex".into(), json!(n));
                }
                Some(Index::Text(t)) => {
                    payload.insert("lastConsumedClientMessageIndex".into(), json!(t));
                }
                Some(Index::Bool(b)) => {
                    payload.insert("lastConsumedClientMessageIndex".into(), json!(b));
                }
                None => {}
            }
            json!({ "sessionResumptionUpdate": Value::Object(payload) })
        }
        Frame::VoiceActivity { kind, offset } => {
            let mut payload = serde_json::Map::new();
            let kind = match kind {
                Some(Activity::Start) => Some("ACTIVITY_START".to_string()),
                Some(Activity::End) => Some("ACTIVITY_END".to_string()),
                Some(Activity::LegacyStart) => Some("VOICE_ACTIVITY_START".to_string()),
                Some(Activity::Unknown(s)) => Some(s.clone()),
                None => None,
            };
            if let Some(k) = kind {
                payload.insert("voiceActivityType".into(), json!(k));
            }
            if let Some(o) = offset {
                payload.insert("audioOffset".into(), json!(o));
            }
            json!({ "voiceActivity": Value::Object(payload) })
        }
        Frame::UsageOnly(total) => json!({ "usageMetadata": { "totalTokenCount": total } }),
        Frame::Unknown(key, value) => json!({ key: value.to_json(0) }),
        Frame::Nested(bytes) => {
            // A frame wrapped in an array or a bare scalar: not an object.
            json!([String::from_utf8_lossy(bytes)])
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let Ok(frame) = Frame::arbitrary(&mut u) else {
        return;
    };
    let json = frame_json(&frame);
    let text = json.to_string();
    let decoded = JsonCodec.decode_message(text.as_bytes());

    match (&frame, decoded) {
        (Frame::SetupComplete, Ok(ServerMessage::SetupComplete(_))) => {}
        (Frame::Content { parts, .. }, Ok(ServerMessage::ServerContent(content))) => {
            // Every audio part survives base64 unchanged, in order.
            let expected: Vec<&Vec<u8>> = parts
                .iter()
                .take(16)
                .filter_map(|p| match p {
                    Part::Audio { data, .. } => Some(data),
                    _ => None,
                })
                .collect();
            if let Some(turn) = &content.server_content.model_turn {
                let got: Vec<Vec<u8>> = turn
                    .parts
                    .iter()
                    .filter_map(|p| {
                        serde_json::to_value(p)
                            .ok()
                            .and_then(|v| v.pointer("/inlineData/data").and_then(Value::as_str).map(str::to_owned))
                    })
                    .map(|b64| base64::engine::general_purpose::STANDARD.decode(b64).expect("the decoder keeps data base64"))
                    .collect();
                assert_eq!(got.len(), expected.len(), "every audio part is kept: {text}");
                for (g, e) in got.iter().zip(expected) {
                    assert_eq!(g, e, "audio bytes survive the frame: {text}");
                }
            } else {
                assert!(expected.is_empty(), "audio parts need a model turn: {text}");
            }
        }
        (Frame::ToolCall(calls), Ok(ServerMessage::ToolCall(msg))) => {
            assert_eq!(msg.tool_call.function_calls.len(), calls.len().min(16), "{text}");
        }
        (Frame::ToolCallCancellation(ids), Ok(ServerMessage::ToolCallCancellation(msg))) => {
            assert_eq!(&msg.tool_call_cancellation.ids, ids, "{text}");
        }
        (Frame::GoAway(_), Ok(ServerMessage::GoAway(_))) => {}
        (Frame::SessionResumption { handle, .. }, Ok(ServerMessage::SessionResumptionUpdate(msg))) => {
            assert_eq!(&msg.session_resumption_update.new_handle, handle, "{text}");
        }
        (Frame::VoiceActivity { .. }, Ok(ServerMessage::VoiceActivity(_))) => {}
        (Frame::UsageOnly(total), Ok(ServerMessage::ServerContent(content))) => {
            let usage = content.usage_metadata.expect("usage-only frames keep their usage");
            assert_eq!(usage.total_token_count, Some(*total), "{text}");
        }
        (Frame::Unknown(key, _), Ok(decoded)) => {
            // A key that happens to be a known one is routed; any other key
            // must come back as Unknown rather than fail the frame.
            const KNOWN: [&str; 8] = [
                "setupComplete", "serverContent", "usageMetadata", "toolCall",
                "toolCallCancellation", "goAway", "sessionResumptionUpdate", "voiceActivity",
            ];
            if !KNOWN.contains(&key.as_str()) {
                assert!(matches!(decoded, ServerMessage::Unknown(_)), "{text}");
            }
        }
        (Frame::Nested(_), Ok(ServerMessage::Unknown(_))) => {}
        (Frame::Unknown(key, _), Err(_)) => {
            // A known key with an unexpected payload shape is a parse error
            // by design; an unknown key never is.
            const KNOWN: [&str; 8] = [
                "setupComplete", "serverContent", "usageMetadata", "toolCall",
                "toolCallCancellation", "goAway", "sessionResumptionUpdate", "voiceActivity",
            ];
            assert!(KNOWN.contains(&key.as_str()), "an unknown key must not fail the frame: {text}");
        }
        (frame, other) => panic!("frame {frame:?} decoded as {other:?}: {text}"),
    }
});
