//! Minimal SDP (RFC 8866) offer/answer for G.711 audio calls.
//!
//! A SIP INVITE carries an SDP *offer* describing where the caller wants
//! media sent and which codecs it can speak; the 200 OK carries the *answer*
//! committing to one. This module implements exactly the slice a G.711 voice
//! agent needs: parse the offer's audio media line and connection address,
//! pick μ-law or A-law, and print a well-formed answer. It is deliberately
//! not a general SDP implementation — video sections, ICE, and crypto lines
//! are ignored on the way in and never produced on the way out.

use std::fmt::Write as _;

use super::rtp::{PT_PCMA, PT_PCMU};

/// The audio slice of a parsed SDP offer.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioOffer {
    /// Remote address for RTP, from the media-level or session-level `c=`.
    pub host: String,
    /// Remote RTP port from the `m=audio` line.
    pub port: u16,
    /// Payload types offered on the audio line, in preference order.
    pub payload_types: Vec<u8>,
    /// The dynamic payload type the offer maps to `telephone-event/8000`
    /// (RFC 4733 DTMF), when present. Echoed in the answer so the caller
    /// sends keypresses as events instead of in-band tones.
    pub telephone_event_pt: Option<u8>,
}

impl AudioOffer {
    /// The G.711 payload type to answer with: the offer's preference order,
    /// filtered to what we implement. `None` when the offer has no G.711.
    pub fn g711_payload_type(&self) -> Option<u8> {
        self.payload_types
            .iter()
            .copied()
            .find(|&pt| pt == PT_PCMU || pt == PT_PCMA)
    }
}

/// Parse the audio portion of an SDP offer.
///
/// Returns `None` when there is no usable `m=audio` line or no connection
/// address — an offer we cannot answer.
pub fn parse_audio_offer(sdp: &str) -> Option<AudioOffer> {
    let mut session_host: Option<String> = None;
    let mut audio: Option<(u16, Vec<u8>)> = None;
    let mut media_host: Option<String> = None;
    let mut telephone_event_pt: Option<u8> = None;
    let mut in_audio = false;

    for line in sdp.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("c=") {
            // c=IN IP4 203.0.113.5
            let host = rest.split_whitespace().nth(2)?.to_string();
            if in_audio {
                media_host = Some(host);
            } else if session_host.is_none() {
                session_host = Some(host);
            }
        } else if let Some(rest) = line.strip_prefix("a=rtpmap:") {
            // a=rtpmap:101 telephone-event/8000
            if in_audio && telephone_event_pt.is_none() {
                let mut parts = rest.split_whitespace();
                if let (Some(pt), Some(codec)) = (parts.next(), parts.next()) {
                    if codec.eq_ignore_ascii_case("telephone-event/8000") {
                        telephone_event_pt = pt.parse().ok();
                    }
                }
            }
        } else if let Some(rest) = line.strip_prefix("m=") {
            let mut parts = rest.split_whitespace();
            let kind = parts.next()?;
            if kind == "audio" && audio.is_none() {
                in_audio = true;
                let port: u16 = parts.next()?.parse().ok()?;
                let _proto = parts.next()?; // RTP/AVP
                let payload_types = parts.filter_map(|p| p.parse().ok()).collect();
                audio = Some((port, payload_types));
            } else {
                in_audio = false; // a later media section; stop attributing c= lines
            }
        }
    }

    let (port, payload_types) = audio?;
    if port == 0 {
        return None; // port 0 means the stream is refused
    }
    let host = media_host.or(session_host)?;
    // Only meaningful if the audio line actually lists that payload type.
    let telephone_event_pt = telephone_event_pt.filter(|pt| payload_types.contains(pt));
    Some(AudioOffer {
        host,
        port,
        payload_types,
        telephone_event_pt,
    })
}

/// Print an SDP answer committing to one G.711 codec, optionally accepting
/// RFC 4733 telephone events (DTMF) on the payload type the offer proposed.
///
/// `session_id` doubles as the `o=` version; pass something unique per call
/// (a timestamp, a counter). `host`/`port` are where we will receive RTP.
pub fn audio_answer(
    session_id: u64,
    host: &str,
    port: u16,
    payload_type: u8,
    telephone_event_pt: Option<u8>,
) -> String {
    let codec_name = if payload_type == PT_PCMA {
        "PCMA"
    } else {
        "PCMU"
    };
    let mut out = String::new();
    let _ = writeln!(out, "v=0");
    let _ = writeln!(out, "o=- {session_id} {session_id} IN IP4 {host}");
    let _ = writeln!(out, "s=gemini-rs");
    let _ = writeln!(out, "c=IN IP4 {host}");
    let _ = writeln!(out, "t=0 0");
    match telephone_event_pt {
        Some(te) => {
            let _ = writeln!(out, "m=audio {port} RTP/AVP {payload_type} {te}");
            let _ = writeln!(out, "a=rtpmap:{payload_type} {codec_name}/8000");
            let _ = writeln!(out, "a=rtpmap:{te} telephone-event/8000");
            let _ = writeln!(out, "a=fmtp:{te} 0-15");
        }
        None => {
            let _ = writeln!(out, "m=audio {port} RTP/AVP {payload_type}");
            let _ = writeln!(out, "a=rtpmap:{payload_type} {codec_name}/8000");
        }
    }
    let _ = writeln!(out, "a=ptime:20");
    let _ = writeln!(out, "a=sendrecv");
    // SDP requires CRLF line endings on the wire.
    out.replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFFER: &str = "v=0\r\n\
        o=alice 2890844526 2890844526 IN IP4 198.51.100.1\r\n\
        s=call\r\n\
        c=IN IP4 198.51.100.1\r\n\
        t=0 0\r\n\
        m=audio 49170 RTP/AVP 8 0 101\r\n\
        a=rtpmap:8 PCMA/8000\r\n\
        a=rtpmap:0 PCMU/8000\r\n\
        a=rtpmap:101 telephone-event/8000\r\n";

    #[test]
    fn parses_a_typical_softphone_offer() {
        let offer = parse_audio_offer(OFFER).expect("parses");
        assert_eq!(offer.host, "198.51.100.1");
        assert_eq!(offer.port, 49170);
        assert_eq!(offer.payload_types, vec![8, 0, 101]);
        // Offer prefers A-law; we honor its order.
        assert_eq!(offer.g711_payload_type(), Some(PT_PCMA));
        // RFC 4733 DTMF offered on payload type 101.
        assert_eq!(offer.telephone_event_pt, Some(101));
    }

    #[test]
    fn telephone_event_requires_the_media_line_to_list_it() {
        // rtpmap alone is not enough — the m= line must carry the type.
        let sdp = "v=0\r\nc=IN IP4 192.0.2.1\r\nm=audio 4000 RTP/AVP 0\r\n\
                   a=rtpmap:101 telephone-event/8000\r\n";
        assert_eq!(parse_audio_offer(sdp).unwrap().telephone_event_pt, None);
    }

    #[test]
    fn media_level_connection_overrides_session_level() {
        let sdp = "v=0\r\nc=IN IP4 192.0.2.1\r\nm=audio 4000 RTP/AVP 0\r\nc=IN IP4 192.0.2.99\r\n";
        assert_eq!(parse_audio_offer(sdp).unwrap().host, "192.0.2.99");
    }

    #[test]
    fn rejects_offers_without_usable_audio() {
        assert_eq!(parse_audio_offer("v=0\r\ns=x\r\n"), None);
        // Port 0 refuses the stream.
        assert_eq!(
            parse_audio_offer("v=0\r\nc=IN IP4 192.0.2.1\r\nm=audio 0 RTP/AVP 0\r\n"),
            None
        );
        // Video-only offer.
        assert_eq!(
            parse_audio_offer("v=0\r\nc=IN IP4 192.0.2.1\r\nm=video 5000 RTP/AVP 96\r\n"),
            None
        );
    }

    #[test]
    fn no_g711_means_no_answerable_codec() {
        let sdp = "v=0\r\nc=IN IP4 192.0.2.1\r\nm=audio 4000 RTP/AVP 96 97\r\n";
        assert_eq!(
            parse_audio_offer(sdp).unwrap().g711_payload_type(),
            None,
            "opus-only offers are not answerable by a G.711 agent"
        );
    }

    #[test]
    fn answer_is_wellformed_and_crlf_terminated() {
        let answer = audio_answer(7, "203.0.113.9", 40_000, PT_PCMU, None);
        assert!(answer.contains("m=audio 40000 RTP/AVP 0\r\n"));
        assert!(answer.contains("a=rtpmap:0 PCMU/8000\r\n"));
        assert!(answer.contains("c=IN IP4 203.0.113.9\r\n"));
        assert!(!answer.contains("\n\n"));
        assert!(!answer.contains("telephone-event"));
        // Round-trip: our own answer parses as an offer.
        let parsed = parse_audio_offer(&answer).unwrap();
        assert_eq!(parsed.port, 40_000);
        assert_eq!(parsed.g711_payload_type(), Some(PT_PCMU));
    }

    #[test]
    fn answer_echoes_telephone_event_negotiation() {
        let answer = audio_answer(7, "203.0.113.9", 40_000, PT_PCMU, Some(101));
        assert!(answer.contains("m=audio 40000 RTP/AVP 0 101\r\n"));
        assert!(answer.contains("a=rtpmap:101 telephone-event/8000\r\n"));
        assert!(answer.contains("a=fmtp:101 0-15\r\n"));
        // Round-trip: our own answer advertises the event type back.
        let parsed = parse_audio_offer(&answer).unwrap();
        assert_eq!(parsed.telephone_event_pt, Some(101));
        assert_eq!(parsed.g711_payload_type(), Some(PT_PCMU));
    }
}
