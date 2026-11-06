//! End-to-end DTMF delivery: a digit enters at the transport edge (Twilio
//! `dtmf` frame, RFC 4733 RTP packet, in-band tones) and must land in session
//! state under the shared `telephony:` keys, where flow guards read it.

use std::time::Duration;

use gemini_adk_rs::State;
use gemini_adk_rs::flow::Guard;

use super::bridge::{DtmfDeduper, KEY_DTMF, KEY_DTMF_HISTORY, KeypadGuard, record_dtmf};
use super::dtmf::DtmfDetector;
use super::rtp::{self, RtpPacket};
use super::twilio::{CallOptions, TwilioCall, media_frame};
use crate::live::Live;
use crate::testing::{ScriptedRun, ScriptedServer};

fn dtmf(state: &State) -> Option<String> {
    state.get::<String>(KEY_DTMF)
}

fn history(state: &State) -> Option<String> {
    state.get::<String>(KEY_DTMF_HISTORY)
}

/// Poll until `key` holds `want` (the Twilio inbound loop is a spawned task).
async fn wait_for(state: &State, key: &str, want: &str) {
    for _ in 0..200 {
        if state.get::<String>(key).as_deref() == Some(want) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "{key} never became {want:?}; it is {:?}",
        state.get::<String>(key)
    );
}

async fn session() -> ScriptedRun {
    ScriptedServer::new()
        .play(Live::builder().instruction("Answer the phone."))
        .await
        .expect("scripted session connects")
}

/// A `dtmf` frame the way Twilio sends it.
fn twilio_dtmf(sequence: u32, digit: &str) -> String {
    format!(
        r#"{{"event":"dtmf","streamSid":"MZ1","sequenceNumber":"{sequence}","dtmf":{{"track":"inbound_track","digit":"{digit}"}}}}"#
    )
}

// ── a. Twilio ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn twilio_dtmf_frames_land_in_session_state() {
    let run = session().await;
    let call = TwilioCall::attach(run.handle());
    let state = run.state();
    assert_eq!(dtmf(state), None);

    for (n, (digit, want_history)) in [("1", "1"), ("2", "12"), ("#", "12#")].iter().enumerate() {
        call.from_twilio
            .send(twilio_dtmf(n as u32 + 1, digit))
            .await
            .unwrap();
        wait_for(state, KEY_DTMF_HISTORY, want_history).await;
        assert_eq!(dtmf(state).as_deref(), Some(*digit));
    }
    call.abort();
}

#[tokio::test]
async fn twilio_connector_records_every_dtmf_frame_it_is_given() {
    // Twilio's `dtmf` frame carries no event timestamp, only a per-stream
    // sequence number, and the connector does not use `DtmfDeduper` (that is
    // for RFC 4733 retransmissions). A frame with identical content is
    // therefore indistinguishable from the caller pressing the key twice, and
    // is recorded twice. This test pins that behaviour.
    let run = session().await;
    let call = TwilioCall::attach(run.handle());
    let state = run.state();
    let frame = twilio_dtmf(1, "1");
    call.from_twilio.send(frame.clone()).await.unwrap();
    call.from_twilio.send(frame).await.unwrap();
    wait_for(state, KEY_DTMF_HISTORY, "11").await;
    call.abort();
}

// ── b. RTP (RFC 4733) ───────────────────────────────────────────────────────

/// The telephone-event handling of `sip.rs`'s inbound loop, over the same
/// building blocks: parse the datagram, parse the 4-byte payload, deduplicate
/// end-marked events by RTP timestamp, record.
struct RtpEdge {
    pt: u8,
    dedup: DtmfDeduper,
    state: State,
    sequence: u16,
}

impl RtpEdge {
    fn new(state: State) -> Self {
        Self {
            pt: 101,
            dedup: DtmfDeduper::default(),
            state,
            sequence: 0,
        }
    }

    /// Send one telephone-event packet through build -> parse -> record.
    fn packet(&mut self, event: u8, end: bool, duration: u16, timestamp: u32) {
        let mut payload = vec![event, if end { 0x80 | 10 } else { 10 }];
        payload.extend_from_slice(&duration.to_be_bytes());
        self.sequence += 1;
        let datagram = rtp::build(&RtpPacket {
            payload_type: self.pt,
            marker: false,
            sequence: self.sequence,
            timestamp,
            ssrc: 0xABCD,
            payload,
        });
        let packet = rtp::parse(&datagram).expect("well-formed RTP");
        assert_eq!(packet.payload_type, self.pt);
        if let Some(event) = rtp::parse_telephone_event(&packet.payload)
            && self.dedup.accept(event.end, packet.timestamp)
            && let Some(digit) = event.digit()
        {
            record_dtmf(&self.state, digit);
        }
    }
}

#[test]
fn rfc4733_end_packets_record_once_per_event() {
    let state = State::new();
    let mut edge = RtpEdge::new(state.clone());

    // An in-progress event (no end bit) records nothing.
    edge.packet(1, false, 160, 1_000);
    edge.packet(1, false, 320, 1_000);
    assert_eq!(dtmf(&state), None);
    assert_eq!(history(&state), None);

    // End packet, retransmitted three times with the same timestamp.
    for _ in 0..3 {
        edge.packet(1, true, 480, 1_000);
    }
    assert_eq!(dtmf(&state).as_deref(), Some("1"));
    assert_eq!(history(&state).as_deref(), Some("1"));

    // A new timestamp is a new keypress, even for the same digit.
    for _ in 0..3 {
        edge.packet(1, true, 480, 5_000);
    }
    assert_eq!(history(&state).as_deref(), Some("11"));

    // Other events: 2, then * (10), # (11) and A (12).
    edge.packet(2, true, 480, 9_000);
    edge.packet(10, true, 480, 13_000);
    edge.packet(11, true, 480, 17_000);
    edge.packet(12, true, 480, 21_000);
    assert_eq!(dtmf(&state).as_deref(), Some("A"));
    assert_eq!(history(&state).as_deref(), Some("112*#A"));

    // An unknown event code with the end bit records nothing.
    edge.packet(99, true, 480, 25_000);
    assert_eq!(history(&state).as_deref(), Some("112*#A"));
}

// ── c. In-band ──────────────────────────────────────────────────────────────

/// `digit` as a dual tone of `on_ms` followed by `off_ms` of silence, 8 kHz.
fn tone(digit: char, on_ms: usize, off_ms: usize) -> Vec<i16> {
    let (row, col) = match digit {
        '1' => (697.0, 1209.0),
        '5' => (770.0, 1336.0),
        '9' => (852.0, 1477.0),
        '#' => (941.0, 1477.0),
        _ => unreachable!("test only synthesizes 1, 5, 9 and #"),
    };
    let mut out: Vec<i16> = (0..on_ms * 8)
        .map(|n| {
            let t = n as f64 / 8_000.0;
            (6_000.0
                * ((2.0 * std::f64::consts::PI * row * t).sin()
                    + (2.0 * std::f64::consts::PI * col * t).sin())) as i16
        })
        .collect();
    out.extend(std::iter::repeat_n(0, off_ms * 8));
    out
}

#[test]
fn in_band_tones_reach_state_through_the_detector() {
    let state = State::new();
    let mut detector = DtmfDetector::new(8_000);
    let audio = [tone('5', 80, 60), tone('9', 80, 60)].concat();
    for frame in audio.chunks(160) {
        for digit in detector.feed(frame) {
            record_dtmf(&state, digit);
        }
    }
    assert_eq!(dtmf(&state).as_deref(), Some("9"));
    assert_eq!(history(&state).as_deref(), Some("59"));
}

#[test]
fn keypad_guard_records_in_band_digits_only_when_enabled() {
    use crate::voice::InputAudioProcessor as _;
    for (enabled, want) in [(true, Some("5")), (false, None)] {
        let state = State::new();
        let mut guard = KeypadGuard::new(state.clone(), 8_000).record_in_band(enabled);
        for frame in tone('5', 100, 60).chunks(160) {
            let mut frame = frame.to_vec();
            guard.process_frame(&mut frame);
        }
        assert_eq!(dtmf(&state).as_deref(), want, "record_in_band={enabled}");
    }
}

#[tokio::test]
async fn twilio_media_with_in_band_dtmf_records_the_digit() {
    let run = session().await;
    let call = TwilioCall::attach_with(
        run.handle(),
        CallOptions {
            in_band_dtmf: true,
            ..CallOptions::default()
        },
    );
    let state = run.state();
    // Caller audio as Twilio delivers it: 20 ms mu-law frames.
    for frame in tone('1', 100, 60).chunks(160) {
        call.from_twilio
            .send(media_frame("MZ1", frame))
            .await
            .unwrap();
    }
    wait_for(state, KEY_DTMF, "1").await;
    assert_eq!(history(state).as_deref(), Some("1"));
    call.abort();
}

#[tokio::test]
async fn twilio_media_without_in_band_dtmf_does_not_double_count() {
    let run = session().await;
    let call = TwilioCall::attach(run.handle());
    let state = run.state();
    for frame in tone('1', 100, 60).chunks(160) {
        call.from_twilio
            .send(media_frame("MZ1", frame))
            .await
            .unwrap();
    }
    // A later out-of-band frame proves the audio was fully processed first.
    call.from_twilio.send(twilio_dtmf(1, "7")).await.unwrap();
    wait_for(state, KEY_DTMF_HISTORY, "7").await;
    call.abort();
}

// ── d. Guard ────────────────────────────────────────────────────────────────

#[test]
fn guard_on_dtmf_flips_when_the_digit_lands() {
    let state = State::new();
    let guard = Guard::eq("telephony:dtmf", "1");
    assert!(!guard.eval_state(&state), "false before any digit");

    record_dtmf(&state, '2');
    assert!(!guard.eval_state(&state), "false for a different digit");

    record_dtmf(&state, '1');
    assert!(guard.eval_state(&state), "true once 1 lands");

    record_dtmf(&state, '3');
    assert!(
        !guard.eval_state(&state),
        "KEY_DTMF is the latest digit only"
    );
}

#[tokio::test]
async fn guard_flips_for_a_digit_delivered_over_twilio() {
    let run = session().await;
    let call = TwilioCall::attach(run.handle());
    let guard = Guard::eq("telephony:dtmf", "1");
    assert!(!guard.eval_state(run.state()));
    call.from_twilio.send(twilio_dtmf(1, "1")).await.unwrap();
    wait_for(run.state(), KEY_DTMF, "1").await;
    assert!(guard.eval_state(run.state()));
    call.abort();
}
