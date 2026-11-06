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
///
/// G.711 defines the encoder on a 14-bit input; the 16-bit sample is taken
/// to that domain with an arithmetic shift, the way the ITU-T G.191 STL and
/// the Sun `g711.c` reference do, so a negative sample rounds toward
/// negative infinity (`-1` encodes as the smallest negative step, `0x7E`,
/// not as negative zero). Bit-exact with that reference for every input.
pub fn linear_to_ulaw(sample: i16) -> u8 {
    const BIAS: i32 = 0x84 >> 2; // 33: the G.711 bias in the 14-bit domain
    const CLIP: i32 = 8_159;
    // Upper bound of each segment in the biased 14-bit domain.
    const SEG_END: [i32; 8] = [0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF, 0x1FFF];

    let value = i32::from(sample) >> 2;
    let (magnitude, mask) = if value < 0 {
        (-value, 0x7Fu8)
    } else {
        (value, 0xFFu8)
    };
    let magnitude = magnitude.min(CLIP) + BIAS;
    let segment = SEG_END.iter().position(|&end| magnitude <= end);
    let Some(segment) = segment else {
        return 0x7F ^ mask;
    };
    let mantissa = ((magnitude >> (segment + 1)) & 0x0F) as u8;
    // μ-law transmits the byte inverted (all-ones is silence on the wire).
    (((segment as u8) << 4) | mantissa) ^ mask
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
///
/// G.711 defines the encoder on a 13-bit input; the 16-bit sample is taken
/// to that domain with an arithmetic shift and negatives are encoded as
/// `-v - 1`, the way the ITU-T G.191 STL and the Sun `g711.c` reference do.
/// Bit-exact with that reference for every input.
pub fn linear_to_alaw(sample: i16) -> u8 {
    // Upper bound of each segment in the 13-bit domain.
    const SEG_END: [i32; 8] = [0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF];

    let value = i32::from(sample) >> 3;
    // A-law XORs alternate bits on the wire (0x55), and the sign bit is set
    // for non-negative samples: the two masks fold both into one XOR.
    let (magnitude, mask) = if value >= 0 {
        (value, 0xD5u8)
    } else {
        (-value - 1, 0x55u8)
    };
    let segment = SEG_END.iter().position(|&end| magnitude <= end);
    let Some(segment) = segment else {
        return 0x7F ^ mask;
    };
    let shift = if segment < 2 { 1 } else { segment };
    let mantissa = ((magnitude >> shift) & 0x0F) as u8;
    (((segment as u8) << 4) | mantissa) ^ mask
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
                let encoded = !((segment << 4) | mantissa);
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
                pos_rt as i32 + neg_rt as i32,
                0,
                "μ-law should preserve sign symmetry: {pos_rt} + {neg_rt} != 0"
            );
        }
    }

    /// A-law's sign symmetry is the reference's, not naive negation: a
    /// two's-complement input has no negative zero, so `-x` encodes as the
    /// mirror of `x - 1` (ITU-T G.191 `alaw_compress` takes `~x`, the Sun
    /// reference `-x - 1`). At an exact segment boundary such as ±256 the
    /// two reconstructions therefore differ by one step (264 and -248), and
    /// the decoder is exactly odd: mirrored codes decode to negated values.
    #[test]
    fn alaw_negative_values_mirror_the_reference() {
        for x in 1..=i16::MAX {
            assert_eq!(
                linear_to_alaw(-x) ^ 0x80,
                linear_to_alaw(x - 1),
                "A-law: -{x} must mirror {}",
                x - 1
            );
        }
        for byte in 0..=u8::MAX {
            assert_eq!(
                i32::from(alaw_to_linear(byte ^ 0x80)),
                -i32::from(alaw_to_linear(byte)),
                "A-law decoder must be odd at byte {byte:#04x}"
            );
        }
        // Away from a boundary the round trips are still negatives of each
        // other, which is what a listener hears.
        for &abs_val in &[100i16, 1000, 8000, 20000] {
            let pos_rt = alaw_to_linear(linear_to_alaw(abs_val));
            let neg_rt = alaw_to_linear(linear_to_alaw(-abs_val));
            assert_eq!(i32::from(pos_rt) + i32::from(neg_rt), 0, "{abs_val}");
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

/// Comparison against an independent transcription of the classic Sun
/// Microsystems `g711.c` reference (public domain; the same algorithm ships
/// with sox, ffmpeg's `g711` tables and libsrtp's test helpers).
#[cfg(test)]
mod reference_tests {
    use super::*;

    // ---- Reference: Sun Microsystems g711.c (public domain) ---------------
    // Inputs/outputs are 16-bit linear PCM, the same scale as this crate:
    // the reference shifts right by 3 (A-law, 13-bit) or 2 (µ-law, 14-bit)
    // internally and its decoders return 16-bit-scale values (A-law silence
    // decodes to ±8, µ-law full scale to ±32124).
    const SIGN_BIT: i32 = 0x80;
    const QUANT_MASK: i32 = 0xf;
    const SEG_SHIFT: i32 = 4;
    const SEG_MASK: i32 = 0x70;
    const BIAS: i32 = 0x84;
    const CLIP: i32 = 8159;
    const SEG_AEND: [i32; 8] = [0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF];
    const SEG_UEND: [i32; 8] = [0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF, 0x1FFF];

    fn search(val: i32, table: &[i32; 8]) -> i32 {
        for (i, &t) in table.iter().enumerate() {
            if val <= t {
                return i as i32;
            }
        }
        8
    }

    fn ref_linear2alaw(pcm: i16) -> u8 {
        let mut pcm_val = (pcm as i32) >> 3; // arithmetic shift, as in C
        let mask;
        if pcm_val >= 0 {
            mask = 0xD5;
        } else {
            mask = 0x55;
            pcm_val = -pcm_val - 1;
        }
        let seg = search(pcm_val, &SEG_AEND);
        if seg >= 8 {
            (0x7F ^ mask) as u8
        } else {
            let mut aval = seg << SEG_SHIFT;
            if seg < 2 {
                aval |= (pcm_val >> 1) & QUANT_MASK;
            } else {
                aval |= (pcm_val >> seg) & QUANT_MASK;
            }
            (aval ^ mask) as u8
        }
    }

    fn ref_alaw2linear(a: u8) -> i16 {
        let a_val = (a as i32) ^ 0x55;
        let mut t = (a_val & QUANT_MASK) << 4;
        let seg = (a_val & SEG_MASK) >> SEG_SHIFT;
        match seg {
            0 => t += 8,
            1 => t += 0x108,
            _ => {
                t += 0x108;
                t <<= seg - 1;
            }
        }
        (if a_val & SIGN_BIT != 0 { t } else { -t }) as i16
    }

    fn ref_linear2ulaw(pcm: i16) -> u8 {
        let mut pcm_val = (pcm as i32) >> 2; // arithmetic shift, as in C
        let mask;
        if pcm_val < 0 {
            pcm_val = -pcm_val;
            mask = 0x7F;
        } else {
            mask = 0xFF;
        }
        if pcm_val > CLIP {
            pcm_val = CLIP;
        }
        pcm_val += BIAS >> 2;
        let seg = search(pcm_val, &SEG_UEND);
        if seg >= 8 {
            (0x7F ^ mask) as u8
        } else {
            let uval = (seg << 4) | ((pcm_val >> (seg + 1)) & 0xF);
            (uval ^ mask) as u8
        }
    }

    fn ref_ulaw2linear(u: u8) -> i16 {
        let u_val = !(u as i32) & 0xFF;
        let mut t = ((u_val & QUANT_MASK) << 3) + BIAS;
        t <<= (u_val & SEG_MASK) >> SEG_SHIFT;
        (if u_val & SIGN_BIT != 0 {
            BIAS - t
        } else {
            t - BIAS
        }) as i16
    }

    #[test]
    fn ulaw_decoder_matches_reference_for_all_bytes() {
        let bad: Vec<String> = (0..=255u8)
            .filter(|&b| ulaw_to_linear(b) != ref_ulaw2linear(b))
            .map(|b| {
                format!(
                    "byte {b:#04x}: crate {} reference {}",
                    ulaw_to_linear(b),
                    ref_ulaw2linear(b)
                )
            })
            .collect();
        assert!(bad.is_empty(), "{} disagreements: {bad:#?}", bad.len());
    }

    #[test]
    fn alaw_decoder_matches_reference_for_all_bytes() {
        let bad: Vec<String> = (0..=255u8)
            .filter(|&b| alaw_to_linear(b) != ref_alaw2linear(b))
            .map(|b| {
                format!(
                    "byte {b:#04x}: crate {} reference {}",
                    alaw_to_linear(b),
                    ref_alaw2linear(b)
                )
            })
            .collect();
        assert!(bad.is_empty(), "{} disagreements: {bad:#?}", bad.len());
    }

    #[test]
    fn ulaw_encoder_matches_reference_for_every_i16() {
        let bad: Vec<String> = (i16::MIN..=i16::MAX)
            .filter(|&s| linear_to_ulaw(s) != ref_linear2ulaw(s))
            .map(|s| {
                format!(
                    "sample {s}: crate {:#04x} reference {:#04x}",
                    linear_to_ulaw(s),
                    ref_linear2ulaw(s)
                )
            })
            .collect();
        assert!(
            bad.is_empty(),
            "{} of 65536 disagree; first 20: {:#?}",
            bad.len(),
            &bad[..bad.len().min(20)]
        );
    }

    #[test]
    fn alaw_encoder_matches_reference_for_every_i16() {
        let bad: Vec<String> = (i16::MIN..=i16::MAX)
            .filter(|&s| linear_to_alaw(s) != ref_linear2alaw(s))
            .map(|s| {
                format!(
                    "sample {s}: crate {:#04x} reference {:#04x}",
                    linear_to_alaw(s),
                    ref_linear2alaw(s)
                )
            })
            .collect();
        assert!(
            bad.is_empty(),
            "{} of 65536 disagree; first 20: {:#?}",
            bad.len(),
            &bad[..bad.len().min(20)]
        );
    }

    #[test]
    fn known_ulaw_pairs() {
        // (wire byte, linear PCM16 value) on the crate's 16-bit scale.
        let decode: [(u8, i16); 6] = [
            (0xFF, 0),       // silence
            (0x7F, 0),       // negative zero
            (0x00, -32_124), // most negative
            (0x80, 32_124),  // most positive
            (0xFE, 8),
            (0x7E, -8),
        ];
        for (byte, pcm) in decode {
            assert_eq!(ulaw_to_linear(byte), pcm, "decode {byte:#04x}");
        }
        // Encoder direction: silence, full-scale clipping, smallest steps.
        let encode: [(i16, u8); 6] = [
            (0, 0xFF),
            (32_767, 0x80),
            (-32_768, 0x00),
            (8, 0xFE),
            (-8, 0x7E),
            (-1, 0x7E), // floor(-1/4) = -1 in the reference: rounds away from zero
        ];
        for (pcm, byte) in encode {
            assert_eq!(linear_to_ulaw(pcm), byte, "encode {pcm}");
        }
    }

    #[test]
    fn known_alaw_pairs() {
        let decode: [(u8, i16); 6] = [
            (0xD5, 8), // A-law silence byte (0x55 ^ 0x80 ^ ...)
            (0x55, -8),
            (0xAA, 32_256),  // largest positive
            (0x2A, -32_256), // largest negative
            (0xD4, 24),
            (0x54, -24),
        ];
        for (byte, pcm) in decode {
            assert_eq!(alaw_to_linear(byte), pcm, "decode {byte:#04x}");
        }
        let encode: [(i16, u8); 6] = [
            (0, 0xD5),
            (32_767, 0xAA),
            (-32_768, 0x2A),
            (8, 0xD5),
            (-8, 0x55),
            (-1, 0x55),
        ];
        for (pcm, byte) in encode {
            assert_eq!(linear_to_alaw(pcm), byte, "encode {pcm}");
        }
    }
}
