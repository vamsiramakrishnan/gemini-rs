//! Raw bytes through the Live frame decoder, on both wire paths.
//!
//! Google AI sends JSON in text frames and Vertex AI sends the same JSON in
//! binary frames. The transport hands both to the codec as bytes
//! (`transport::ws::frame_payload`), so one input exercises both paths: the
//! bytes are wrapped as a text frame when they are valid UTF-8 and as a
//! binary frame otherwise, then decoded. The decoder must never panic, and
//! the two paths must agree on every input that is valid UTF-8.
#![no_main]

use gemini_genai_rs::transport::ws::{FramePayload, frame_payload};
use gemini_genai_rs::transport::{Codec, JsonCodec};
use libfuzzer_sys::fuzz_target;
use tokio_tungstenite::tungstenite::Message;

fuzz_target!(|data: &[u8]| {
    let codec = JsonCodec;

    // The binary (Vertex AI) path: bytes arrive as they are.
    let binary = frame_payload(Message::Binary(data.to_vec().into()));
    let FramePayload::Data(binary_bytes) = binary else {
        panic!("a binary frame is always data");
    };
    let binary_result = codec.decode_message(&binary_bytes);

    // The text (Google AI) path exists only for valid UTF-8.
    if let Ok(text) = std::str::from_utf8(data) {
        let text_frame = frame_payload(Message::Text(text.into()));
        let FramePayload::Data(text_bytes) = text_frame else {
            panic!("a text frame is always data");
        };
        assert_eq!(text_bytes, binary_bytes, "the two paths carry the same bytes");
        let text_result = codec.decode_message(&text_bytes);
        assert_eq!(
            text_result.is_ok(),
            binary_result.is_ok(),
            "text and binary paths must agree on whether a frame decodes"
        );
    } else {
        assert!(
            binary_result.is_err(),
            "non-UTF-8 bytes can never decode as a JSON frame"
        );
    }

    // Close and control frames never reach the codec.
    assert!(matches!(
        frame_payload(Message::Close(None)),
        FramePayload::Close(None)
    ));
    assert!(matches!(
        frame_payload(Message::Ping(data.to_vec().into())),
        FramePayload::Skip
    ));
});
