//! G.711 μ-law and A-law codecs — the audio dialect of the public phone
//! network.
//!
//! Every PSTN leg (Twilio Media Streams, SIP trunks, carrier gateways)
//! delivers 8 kHz audio in one of these two companding formats: μ-law in
//! North America and Japan, A-law nearly everywhere else. Both pack a
//! 14/13-bit linear sample into one logarithmically-companded byte.
//!
//! The functions here are pure, allocation-explicit, and implement ITU-T
//! G.711 directly (bit manipulation, no tables to drift): encode from mono
//! PCM16, decode back to mono PCM16. Pair them with
//! [`resample`](crate::voice::resample) to move between the 8 kHz telephone
//! rate and the Live API's 16 kHz-in / 24 kHz-out contract.

/// Encode one linear PCM16 sample as a μ-law byte (ITU-T G.711).
pub fn linear_to_ulaw(sample: i16) -> u8 {
    const BIAS: i32 = 0x84; // 132: shifts the segment boundaries per G.711
    const CLIP: i32 = 32_635;

    let sign: u8 = if sample < 0 { 0x80 } else { 0 };
    let mut magnitude = (sample as i32).abs().min(CLIP) + BIAS;

    // Segment: how far the magnitude's top bit sits above the base segment
    // (segment 0 spans biased magnitudes up to 0xFF).
    let mut segment: u8 = 0;
    let mut probe = magnitude >> 8;
    while probe > 0 && segment < 7 {
        segment += 1;
        probe >>= 1;
    }

    magnitude >>= segment + 3;
    let mantissa = (magnitude & 0x0F) as u8;
    // μ-law transmits the byte inverted (all-ones is silence on the wire).
    !(sign | (segment << 4) | mantissa)
}

/// Decode one μ-law byte to a linear PCM16 sample (ITU-T G.711).
pub fn ulaw_to_linear(byte: u8) -> i16 {
    let byte = !byte;
    let sign = byte & 0x80;
    let segment = (byte >> 4) & 0x07;
    let mantissa = byte & 0x0F;

    let magnitude = ((((mantissa as i32) << 3) + 0x84) << segment) - 0x84;
    if sign != 0 {
        -magnitude as i16
    } else {
        magnitude as i16
    }
}

/// Encode one linear PCM16 sample as an A-law byte (ITU-T G.711).
pub fn linear_to_alaw(sample: i16) -> u8 {
    const CLIP: i32 = 32_635;

    let sign: u8 = if sample >= 0 { 0x80 } else { 0 };
    let magnitude = (sample as i32).abs().min(CLIP);

    let compressed = if magnitude >= 256 {
        let mut segment: u8 = 1;
        let mut probe = magnitude >> 9;
        while probe > 0 && segment < 7 {
            segment += 1;
            probe >>= 1;
        }
        let mantissa = ((magnitude >> (segment + 3)) & 0x0F) as u8;
        (segment << 4) | mantissa
    } else {
        (magnitude >> 4) as u8
    };

    // A-law XORs alternate bits on the wire.
    (sign | compressed) ^ 0x55
}

/// Decode one A-law byte to a linear PCM16 sample (ITU-T G.711).
pub fn alaw_to_linear(byte: u8) -> i16 {
    let byte = byte ^ 0x55;
    let sign = byte & 0x80;
    let segment = (byte >> 4) & 0x07;
    let mantissa = (byte & 0x0F) as i32;

    let magnitude = match segment {
        0 => (mantissa << 4) + 8,
        _ => ((mantissa << 4) + 0x108) << (segment - 1),
    };
    if sign != 0 {
        magnitude as i16
    } else {
        -magnitude as i16
    }
}

/// Decode a μ-law byte stream to mono PCM16 samples.
pub fn decode_ulaw(bytes: &[u8]) -> Vec<i16> {
    bytes.iter().map(|&b| ulaw_to_linear(b)).collect()
}

/// Encode mono PCM16 samples as a μ-law byte stream.
pub fn encode_ulaw(samples: &[i16]) -> Vec<u8> {
    samples.iter().map(|&s| linear_to_ulaw(s)).collect()
}

/// Decode an A-law byte stream to mono PCM16 samples.
pub fn decode_alaw(bytes: &[u8]) -> Vec<i16> {
    bytes.iter().map(|&b| alaw_to_linear(b)).collect()
}

/// Encode mono PCM16 samples as an A-law byte stream.
pub fn encode_alaw(samples: &[i16]) -> Vec<u8> {
    samples.iter().map(|&s| linear_to_alaw(s)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulaw_silence_is_all_ones_on_the_wire() {
        // The inverted encoding makes digital silence 0xFF — the classic
        // "μ-law idle pattern".
        assert_eq!(linear_to_ulaw(0), 0xFF);
        assert_eq!(ulaw_to_linear(0xFF), 0);
    }

    #[test]
    fn ulaw_known_extremes() {
        // Full-scale positive clips to the largest positive code.
        assert_eq!(ulaw_to_linear(linear_to_ulaw(32_767)), 32_124);
        assert_eq!(ulaw_to_linear(linear_to_ulaw(-32_768)), -32_124);
    }

    #[test]
    fn ulaw_round_trip_is_within_segment_quantisation() {
        // Companding is lossy but monotone: error bounded by the segment's
        // step size (≤ 1/16 of the magnitude, plus the smallest step).
        for &s in &[0i16, 1, -1, 100, -100, 1000, -1000, 8000, -8000, 30000] {
            let rt = ulaw_to_linear(linear_to_ulaw(s));
            let tolerance = (s.unsigned_abs() as i32 / 16).max(16);
            assert!(
                ((rt as i32) - (s as i32)).abs() <= tolerance,
                "sample {s} round-tripped to {rt}"
            );
        }
    }

    #[test]
    fn ulaw_is_monotone() {
        // Decoded values must never decrease as input increases — a codec
        // that reorders amplitudes garbles speech even if errors are small.
        let mut last = i16::MIN;
        for s in (-32_768i32..=32_767).step_by(257) {
            let rt = ulaw_to_linear(linear_to_ulaw(s as i16));
            assert!(rt >= last, "non-monotone at input {s}");
            last = rt;
        }
    }

    #[test]
    fn alaw_round_trip_is_within_segment_quantisation() {
        for &s in &[0i16, 8, -8, 100, -100, 1000, -1000, 8000, -8000, 30000] {
            let rt = alaw_to_linear(linear_to_alaw(s));
            let tolerance = (s.unsigned_abs() as i32 / 16).max(24);
            assert!(
                ((rt as i32) - (s as i32)).abs() <= tolerance,
                "sample {s} round-tripped to {rt}"
            );
        }
    }

    #[test]
    fn alaw_is_monotone() {
        let mut last = i16::MIN;
        for s in (-32_768i32..=32_767).step_by(257) {
            let rt = alaw_to_linear(linear_to_alaw(s as i16));
            assert!(rt >= last, "non-monotone at input {s}");
            last = rt;
        }
    }

    #[test]
    fn stream_helpers_round_trip_shape() {
        let samples = vec![0i16, 500, -500, 12_000, -12_000];
        assert_eq!(decode_ulaw(&encode_ulaw(&samples)).len(), samples.len());
        assert_eq!(decode_alaw(&encode_alaw(&samples)).len(), samples.len());
    }

    #[test]
    fn ulaw_segment_boundaries() {
        // Test round-trip accuracy at segment transitions (where step size changes)
        // Segment boundaries in μ-law are at magnitudes where probe changes (256, 512, 1024, ...)
        for &boundary in &[256i16, 512, 1024, 2048, 4096, 8192, 16384] {
            for offset in &[-1i32, 0, 1] {
                let s = (boundary as i32 + offset) as i16;
                let rt = ulaw_to_linear(linear_to_ulaw(s));
                let tolerance = (s.unsigned_abs() as i32 / 16).max(16);
                assert!(
                    ((rt as i32) - (s as i32)).abs() <= tolerance,
                    "sample {s} at boundary round-tripped to {rt}, error exceeds tolerance"
                );
            }
        }
    }

    #[test]
    fn alaw_segment_boundaries() {
        // Test round-trip accuracy at segment transitions in A-law
        // A-law segment 0 spans magnitude 0-255, then transitions to segment 1
        for &boundary in &[256i16, 512, 1024, 2048, 4096, 8192, 16384] {
            for offset in &[-1i32, 0, 1] {
                let s = (boundary as i32 + offset) as i16;
                let rt = alaw_to_linear(linear_to_alaw(s));
                let tolerance = (s.unsigned_abs() as i32 / 16).max(24);
                assert!(
                    ((rt as i32) - (s as i32)).abs() <= tolerance,
                    "alaw sample {s} at boundary round-tripped to {rt}, error exceeds tolerance"
                );
            }
        }
    }

    #[test]
    fn ulaw_all_mantissas_per_segment() {
        // Verify each mantissa value (0-15) in each segment (0-7) codes/decodes correctly
        for segment in 0u8..=7 {
            for mantissa in 0u8..=15 {
                // Reconstruct the encoded byte: !(sign | segment<<4 | mantissa)
                let encoded = !(0 | (segment << 4) | mantissa);
                let decoded = ulaw_to_linear(encoded);
                // Re-encode to verify round-trip consistency
                let re_encoded = linear_to_ulaw(decoded);
                assert_eq!(
                    re_encoded, encoded,
                    "mantissa {mantissa} in segment {segment}: round-trip mismatch"
                );
            }
        }
    }

    #[test]
    fn alaw_all_mantissas_per_segment() {
        // Verify A-law mantissas (0-15) in each segment (0-7) code/decode correctly
        for segment in 0u8..=7 {
            for mantissa in 0u8..=15 {
                // Reconstruct the encoded byte per A-law spec
                let compressed = (segment << 4) | mantissa;
                let encoded = (0x80 | compressed) ^ 0x55; // with sign bit + XOR
                let decoded = alaw_to_linear(encoded);
                // Re-encode to verify consistency
                let re_encoded = linear_to_alaw(decoded);
                assert_eq!(
                    re_encoded, encoded,
                    "alaw mantissa {mantissa} in segment {segment}: round-trip mismatch"
                );
            }
        }
    }

    #[test]
    fn ulaw_negative_values_symmetric() {
        // Negative and positive values should have symmetric round-trip errors
        for &abs_val in &[100i16, 256, 1000, 8000, 20000] {
            let pos_rt = ulaw_to_linear(linear_to_ulaw(abs_val));
            let neg_rt = ulaw_to_linear(linear_to_ulaw(-abs_val));
            assert_eq!(
                pos_rt as i32 + neg_rt as i32, 0,
                "μ-law should preserve sign symmetry: {} + {} != 0",
                pos_rt, neg_rt
            );
        }
    }

    #[test]
    fn alaw_negative_values_symmetric() {
        // A-law should also preserve sign symmetry
        for &abs_val in &[100i16, 256, 1000, 8000, 20000] {
            let pos_rt = alaw_to_linear(linear_to_alaw(abs_val));
            let neg_rt = alaw_to_linear(linear_to_alaw(-abs_val));
            assert_eq!(
                pos_rt as i32 + neg_rt as i32, 0,
                "A-law should preserve sign symmetry: {} + {} != 0",
                pos_rt, neg_rt
            );
        }
    }

    #[test]
    fn ulaw_clipping_at_extremes() {
        // Values beyond CLIP (32635) should clip to the same rounded value
        let large_pos = ulaw_to_linear(linear_to_ulaw(32_767));
        let large_pos2 = ulaw_to_linear(linear_to_ulaw(32_700));
        assert_eq!(
            large_pos, large_pos2,
            "μ-law should clip large positive values to same result"
        );

        let large_neg = ulaw_to_linear(linear_to_ulaw(-32_768));
        let large_neg2 = ulaw_to_linear(linear_to_ulaw(-32_700));
        assert_eq!(
            large_neg, large_neg2,
            "μ-law should clip large negative values to same result"
        );
    }

    #[test]
    fn alaw_clipping_at_extremes() {
        // A-law should also clip consistently
        let large_pos = alaw_to_linear(linear_to_alaw(32_767));
        let large_pos2 = alaw_to_linear(linear_to_alaw(32_700));
        assert_eq!(
            large_pos, large_pos2,
            "A-law should clip large positive values to same result"
        );

        let large_neg = alaw_to_linear(linear_to_alaw(-32_768));
        let large_neg2 = alaw_to_linear(linear_to_alaw(-32_700));
        assert_eq!(
            large_neg, large_neg2,
            "A-law should clip large negative values to same result"
        );
    }
}
