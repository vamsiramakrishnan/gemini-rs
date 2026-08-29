//! RTP (RFC 3550) — the media framing of every SIP call.
//!
//! A raw SIP/PSTN leg carries audio as RTP packets over UDP: a 12-byte fixed
//! header (sequence number, timestamp, SSRC) followed by the codec payload —
//! for telephone audio, G.711 μ-law (payload type 0, `PCMU`) or A-law
//! (payload type 8, `PCMA`) at 8 kHz, conventionally 20 ms (160 samples) per
//! packet.
//!
//! This module is the pure layer: [`build`] and [`parse`] move between packet
//! bytes and structured form (tolerating padding, CSRCs, and header
//! extensions on the way in), and [`RtpSender`] carries the tiny amount of
//! state a sender needs (sequence, timestamp, SSRC). No sockets — the `sip`
//! feature's media loop drives it over UDP, and tests drive it with byte
//! arrays.

/// RTP payload type for G.711 μ-law at 8 kHz (RFC 3551 static assignment).
pub const PT_PCMU: u8 = 0;
/// RTP payload type for G.711 A-law at 8 kHz (RFC 3551 static assignment).
pub const PT_PCMA: u8 = 8;

/// Samples per packet at the conventional 20 ms packetisation (8 kHz mono).
pub const SAMPLES_PER_PACKET: usize = 160;

/// One parsed RTP packet (header fields we care about + payload bytes).
#[derive(Debug, Clone, PartialEq)]
pub struct RtpPacket {
    /// Payload type (e.g. [`PT_PCMU`], [`PT_PCMA`]).
    pub payload_type: u8,
    /// Marker bit — set on the first packet after silence (talkspurt start).
    pub marker: bool,
    /// Sequence number, increments by one per packet.
    pub sequence: u16,
    /// Media timestamp in samples (8 kHz clock for G.711).
    pub timestamp: u32,
    /// Synchronisation source identifier.
    pub ssrc: u32,
    /// Codec payload bytes.
    pub payload: Vec<u8>,
}

/// Build an RTP packet (version 2, no padding/extension/CSRC).
pub fn build(packet: &RtpPacket) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + packet.payload.len());
    out.push(0x80); // V=2, P=0, X=0, CC=0
    out.push((packet.payload_type & 0x7F) | if packet.marker { 0x80 } else { 0 });
    out.extend_from_slice(&packet.sequence.to_be_bytes());
    out.extend_from_slice(&packet.timestamp.to_be_bytes());
    out.extend_from_slice(&packet.ssrc.to_be_bytes());
    out.extend_from_slice(&packet.payload);
    out
}

/// Parse an RTP packet, tolerating padding, CSRC entries, and a header
/// extension. Returns `None` for datagrams that are not well-formed RTP v2 —
/// a media port sees stray traffic (STUN probes, scans); dropping quietly is
/// the correct posture.
pub fn parse(datagram: &[u8]) -> Option<RtpPacket> {
    if datagram.len() < 12 {
        return None;
    }
    let b0 = datagram[0];
    if b0 >> 6 != 2 {
        return None; // not RTP version 2
    }
    let has_padding = b0 & 0x20 != 0;
    let has_extension = b0 & 0x10 != 0;
    let csrc_count = (b0 & 0x0F) as usize;
    let b1 = datagram[1];

    let mut offset = 12 + csrc_count * 4;
    if datagram.len() < offset {
        return None;
    }
    if has_extension {
        if datagram.len() < offset + 4 {
            return None;
        }
        let ext_words = u16::from_be_bytes([datagram[offset + 2], datagram[offset + 3]]) as usize;
        offset += 4 + ext_words * 4;
        if datagram.len() < offset {
            return None;
        }
    }
    let mut end = datagram.len();
    if has_padding {
        let pad = *datagram.last()? as usize;
        if pad == 0 || offset + pad > end {
            return None;
        }
        end -= pad;
    }

    Some(RtpPacket {
        payload_type: b1 & 0x7F,
        marker: b1 & 0x80 != 0,
        sequence: u16::from_be_bytes([datagram[2], datagram[3]]),
        timestamp: u32::from_be_bytes([datagram[4], datagram[5], datagram[6], datagram[7]]),
        ssrc: u32::from_be_bytes([datagram[8], datagram[9], datagram[10], datagram[11]]),
        payload: datagram[offset..end].to_vec(),
    })
}

// ── Telephone events (RFC 4733 DTMF) ────────────────────────────────────────

/// One parsed telephone-event (RFC 4733) payload — a DTMF keypress carried
/// as RTP instead of audio tones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelephoneEvent {
    /// Event code: 0–9 are the digits, 10 is `*`, 11 is `#`, 12–15 are A–D.
    pub event: u8,
    /// End bit — set on the final packet(s) of the keypress. End packets are
    /// conventionally retransmitted three times; deduplicate on
    /// [`RtpPacket::timestamp`], which stays constant for one keypress.
    pub end: bool,
    /// Cumulative duration of the event so far, in timestamp units.
    pub duration: u16,
}

impl TelephoneEvent {
    /// The event as the character a keypad prints, `None` for codes > 15.
    pub fn digit(&self) -> Option<char> {
        Some(match self.event {
            0..=9 => (b'0' + self.event) as char,
            10 => '*',
            11 => '#',
            12..=15 => (b'A' + self.event - 12) as char,
            _ => return None,
        })
    }
}

/// Parse an RFC 4733 telephone-event payload (the 4-byte named-event form).
///
/// The caller decides *whether* a packet is a telephone event from the
/// payload type negotiated in SDP — this parses the payload of one that is.
pub fn parse_telephone_event(payload: &[u8]) -> Option<TelephoneEvent> {
    if payload.len() < 4 {
        return None;
    }
    Some(TelephoneEvent {
        event: payload[0],
        end: payload[1] & 0x80 != 0,
        duration: u16::from_be_bytes([payload[2], payload[3]]),
    })
}

/// Sender-side RTP state: sequence, timestamp, and SSRC advance per packet.
#[derive(Debug)]
pub struct RtpSender {
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
    /// Set the marker bit on the next packet (start of a talkspurt).
    mark_next: bool,
}

impl RtpSender {
    /// Create a sender for the given payload type and SSRC.
    ///
    /// Callers should pick a random-ish SSRC and initial sequence/timestamp;
    /// determinism here keeps the function pure — randomness is the caller's.
    pub fn new(payload_type: u8, ssrc: u32, initial_sequence: u16, initial_timestamp: u32) -> Self {
        Self {
            payload_type,
            sequence: initial_sequence,
            timestamp: initial_timestamp,
            ssrc,
            mark_next: true,
        }
    }

    /// Frame one payload as an RTP packet and advance sequence/timestamp.
    ///
    /// `samples` is the number of media samples the payload covers (for
    /// G.711, one byte per sample — 160 for a 20 ms packet).
    pub fn packetize(&mut self, payload: &[u8], samples: u32) -> Vec<u8> {
        let packet = build(&RtpPacket {
            payload_type: self.payload_type,
            marker: self.mark_next,
            sequence: self.sequence,
            timestamp: self.timestamp,
            ssrc: self.ssrc,
            payload: payload.to_vec(),
        });
        self.mark_next = false;
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(samples);
        packet
    }

    /// Advance the media clock over a silent gap and mark the next packet as
    /// the start of a new talkspurt.
    pub fn skip_silence(&mut self, samples: u32) {
        self.timestamp = self.timestamp.wrapping_add(samples);
        self.mark_next = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_canonical_wire_layout() {
        let bytes = build(&RtpPacket {
            payload_type: PT_PCMU,
            marker: true,
            sequence: 0x0102,
            timestamp: 0x03040506,
            ssrc: 0x0708090A,
            payload: vec![0xFF, 0xFE],
        });
        assert_eq!(
            bytes,
            vec![
                0x80, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0xFF, 0xFE
            ]
        );
    }

    #[test]
    fn parse_round_trips_build() {
        let packet = RtpPacket {
            payload_type: PT_PCMA,
            marker: false,
            sequence: 65_535,
            timestamp: u32::MAX - 1,
            ssrc: 42,
            payload: vec![1, 2, 3, 4],
        };
        assert_eq!(parse(&build(&packet)), Some(packet));
    }

    #[test]
    fn parse_skips_csrc_extension_and_padding() {
        // V=2, P=1, X=1, CC=1 · PT=0 · seq 1 · ts 2 · ssrc 3
        let mut bytes = vec![0xB1, 0x00, 0x00, 0x01, 0, 0, 0, 2, 0, 0, 0, 3];
        bytes.extend_from_slice(&[9, 9, 9, 9]); // one CSRC
        bytes.extend_from_slice(&[0xBE, 0xDE, 0x00, 0x01, 0, 0, 0, 0]); // ext: 1 word
        bytes.extend_from_slice(&[0xAA, 0xBB]); // payload
        bytes.extend_from_slice(&[0, 0, 3]); // 3 bytes padding (last byte = count)
        let packet = parse(&bytes).expect("valid despite extras");
        assert_eq!(packet.payload, vec![0xAA, 0xBB]);
        assert_eq!(packet.sequence, 1);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&[0x80; 5]), None); // too short
        assert_eq!(parse(&[0x00; 20]), None); // version 0 (e.g. STUN)
    }

    #[test]
    fn telephone_events_parse_digits_and_end_bits() {
        // '5' pressed, not yet released, 160 timestamp units in.
        let event = parse_telephone_event(&[5, 0x0A, 0x00, 0xA0]).unwrap();
        assert_eq!(event.digit(), Some('5'));
        assert!(!event.end);
        assert_eq!(event.duration, 160);

        // '#' released (end bit set).
        let end = parse_telephone_event(&[11, 0x8A, 0x03, 0x20]).unwrap();
        assert_eq!(end.digit(), Some('#'));
        assert!(end.end);

        assert_eq!(
            parse_telephone_event(&[12, 0x80, 0, 60]).unwrap().digit(),
            Some('A')
        );
        assert_eq!(
            parse_telephone_event(&[10, 0x80, 0, 60]).unwrap().digit(),
            Some('*')
        );
        // Flash-hook (16) and other extended events carry no keypad digit.
        assert_eq!(
            parse_telephone_event(&[16, 0x80, 0, 60]).unwrap().digit(),
            None
        );
        // Truncated payload.
        assert_eq!(parse_telephone_event(&[5, 0x80]), None);
    }

    #[test]
    fn sender_advances_and_marks_talkspurts() {
        let mut sender = RtpSender::new(PT_PCMU, 7, 100, 1000);
        let first = parse(&sender.packetize(&[0u8; 160], 160)).unwrap();
        let second = parse(&sender.packetize(&[0u8; 160], 160)).unwrap();
        assert!(first.marker, "first packet starts a talkspurt");
        assert!(!second.marker);
        assert_eq!(second.sequence, 101);
        assert_eq!(second.timestamp, 1160);

        sender.skip_silence(800); // 100 ms of silence
        let resumed = parse(&sender.packetize(&[0u8; 160], 160)).unwrap();
        assert!(resumed.marker, "resuming after silence re-marks");
        // 1000 + 2×160 (sent) + 800 (skipped) = 2120.
        assert_eq!(resumed.timestamp, 2120);
    }

    #[test]
    fn rtp_packet_with_no_payload() {
        // Valid RTP packet with zero-length payload
        let packet = RtpPacket {
            payload_type: PT_PCMU,
            marker: false,
            sequence: 100,
            timestamp: 1000,
            ssrc: 42,
            payload: Vec::new(),
        };
        let built = build(&packet);
        let parsed = parse(&built).expect("should parse empty payload");
        assert_eq!(parsed.payload, Vec::<u8>::new());
    }

    #[test]
    fn rtp_sequence_wrapping() {
        // Sequence numbers should wrap at u16::MAX
        let mut sender = RtpSender::new(PT_PCMU, 1, u16::MAX - 1, 0);
        let p1 = parse(&sender.packetize(&[0u8; 160], 160)).unwrap();
        let p2 = parse(&sender.packetize(&[0u8; 160], 160)).unwrap();
        assert_eq!(p1.sequence, u16::MAX - 1);
        assert_eq!(p2.sequence, u16::MAX);
        let p3 = parse(&sender.packetize(&[0u8; 160], 160)).unwrap();
        assert_eq!(p3.sequence, 0, "sequence should wrap to 0");
    }

    #[test]
    fn rtp_timestamp_wrapping() {
        // Timestamps should wrap at u32::MAX
        let mut sender = RtpSender::new(PT_PCMU, 1, 0, u32::MAX - 100);
        let p1 = parse(&sender.packetize(&[0u8; 160], 160)).unwrap();
        assert_eq!(p1.timestamp, u32::MAX - 100);
        let p2 = parse(&sender.packetize(&[0u8; 160], 160)).unwrap();
        let expected = (u32::MAX as u64 - 100 + 160) as u32;
        assert_eq!(p2.timestamp, expected, "timestamp should wrap correctly");
    }

    #[test]
    fn rtp_csrc_count_limits() {
        // Max CSRC count is 15 (4-bit field)
        let mut bytes = vec![0x8F, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]; // V=2, CC=15, no P/X
        for _ in 0..15 {
            bytes.extend_from_slice(&[0, 0, 0, 0]); // 15 CSRCs
        }
        bytes.extend_from_slice(&[1, 2]); // payload
        let packet = parse(&bytes).expect("should parse max CSRCs");
        assert_eq!(packet.payload, vec![1, 2]);
    }

    #[test]
    fn rtp_extension_header_handling() {
        // RTP extension with multiple 4-byte words
        let mut bytes = vec![0x90, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]; // no CSRC, has ext
        bytes.extend_from_slice(&[0xAB, 0xCD, 0x00, 0x04]); // ext: profile, 4 words
        for _ in 0..4 {
            bytes.extend_from_slice(&[0xFF, 0xEE, 0xDD, 0xCC]);
        }
        bytes.extend_from_slice(&[0x12, 0x34]); // payload
        let packet = parse(&bytes).expect("should parse extension");
        assert_eq!(packet.payload, vec![0x12, 0x34]);
    }

    #[test]
    fn rtp_padding_edge_cases() {
        // Padding: payload [1, 2, 3] followed by 3-byte padding (includes count)
        let mut bytes = vec![0xA0, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]; // P=1, X=0, CC=0
        bytes.extend_from_slice(&[1, 2]); // payload
        bytes.extend_from_slice(&[0, 0, 3]); // 3 bytes padding (0, 0, and count byte 3)
        let packet = parse(&bytes).expect("should parse when pad equals end");
        assert_eq!(packet.payload, vec![1, 2]); // last 3 bytes removed as padding
    }

    #[test]
    fn rtp_parse_rejects_invalid_padding() {
        // Padding count of 0 is invalid
        let mut bytes = vec![0xA0, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]; // P=1
        bytes.extend_from_slice(&[1, 2]);
        bytes.push(0); // invalid: padding count must be >= 1
        assert_eq!(parse(&bytes), None, "should reject zero padding count");

        // Padding extends beyond datagram
        let mut bytes = vec![0xA0, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]; // P=1
        bytes.extend_from_slice(&[1, 2]);
        bytes.push(10); // padding count = 10, but only 3 bytes follow the header
        assert_eq!(parse(&bytes), None, "should reject oversized padding");
    }

    #[test]
    fn telephone_event_all_digits() {
        // Test all standard DTMF codes (0-15)
        for event_code in 0u8..=15 {
            let payload = [event_code, 0x00, 0x00, 0xA0];
            let event = parse_telephone_event(&payload).unwrap();
            assert_eq!(event.event, event_code);
            assert_eq!(event.end, false);
            // Verify digit() returns appropriate char for 0-11, None for 12-15
            match event_code {
                0..=9 => {
                    assert!(event.digit().is_some());
                }
                10..=11 => {
                    assert!(event.digit().is_some());
                }
                12..=15 => {
                    assert!(event.digit().is_some());
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn telephone_event_duration_limits() {
        // Duration is u16, test edge values
        let payload = [5, 0x80, 0xFF, 0xFF]; // max duration
        let event = parse_telephone_event(&payload).unwrap();
        assert_eq!(event.duration, u16::MAX);

        let payload = [5, 0x80, 0x00, 0x01]; // min non-zero duration
        let event = parse_telephone_event(&payload).unwrap();
        assert_eq!(event.duration, 1);
    }

    #[test]
    fn payload_type_only_uses_7_bits() {
        // Payload type is 7 bits; marker is bit 7
        let packet = RtpPacket {
            payload_type: 0x7F, // max 7-bit value
            marker: true,
            sequence: 0,
            timestamp: 0,
            ssrc: 0,
            payload: vec![1, 2],
        };
        let built = build(&packet);
        let parsed = parse(&built).unwrap();
        assert_eq!(parsed.payload_type, 0x7F);
        assert_eq!(parsed.marker, true);
    }
}
