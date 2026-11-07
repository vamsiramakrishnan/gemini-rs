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
/// The rule is ITU-T G.191's `ulaw_compress` (the STL reference
/// implementation of G.711): the 16-bit sample is taken to the 14-bit
/// domain with an arithmetic shift, a negative sample's magnitude is its
/// one's complement (so `-1` encodes as negative zero, `0x7F`, and `-4` as
/// the first negative step), the bias of 33 is added, and the segment and
/// mantissa are read off the biased value. Bit-exact with G.191 for every
/// input (`reference_tests`). The Sun `g711.c` formulation used by sox and
/// Asterisk negates instead of complementing and differs from G.191 by one
/// code at negative step boundaries; decoders agree on both.
pub fn linear_to_ulaw(sample: i16) -> u8 {
    const BIAS: i32 = 33; // the G.711 bias (0x84) in the 14-bit domain
    const CLIP: i32 = 0x1FFF;

    let value = i32::from(sample);
    let absno = if value < 0 { (!value) >> 2 } else { value >> 2 } + BIAS;
    let absno = absno.min(CLIP);
    // Segment: one plus the number of significant bits above the lowest six.
    let segno = 1 + (32 - (absno >> 6).leading_zeros()) as i32;
    let high_nibble = 8 - segno;
    let low_nibble = 0x0F - ((absno >> segno) & 0x0F);
    let byte = ((high_nibble << 4) | low_nibble) as u8;
    // The sign bit is set for non-negative samples (all-ones is silence).
    if value >= 0 { byte | 0x80 } else { byte }
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
/// to that domain with an arithmetic shift and a negative sample's
/// magnitude is `-v - 1`, which equals the one's complement ITU-T G.191's
/// `alaw_compress` takes. Bit-exact with G.191 for every input
/// (`reference_tests`).
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

    /// μ-law's sign symmetry is G.191's, not naive negation: a negative
    /// sample's magnitude is its one's complement, so `-x` encodes as the
    /// mirror of `x - 1`, and the round trip of `-x` is the negation of the
    /// round trip of `x - 1`.
    #[test]
    fn ulaw_negative_values_mirror_the_reference() {
        for x in 1..=i16::MAX {
            assert_eq!(
                linear_to_ulaw(-x) ^ 0x80,
                linear_to_ulaw(x - 1),
                "μ-law mirror relation at {x}"
            );
            assert_eq!(
                i32::from(ulaw_to_linear(linear_to_ulaw(-x))),
                -i32::from(ulaw_to_linear(linear_to_ulaw(x - 1))),
                "μ-law round-trip mirror at {x}"
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

/// Comparison against a transcription of ITU-T G.191's `g711.c` (the STL
/// reference implementation of G.711, version 3.01, fetched from the ITU's
/// repository): every encoder and decoder is compared for every input, the
/// decoders' outputs round-trip through the encoders, and a handful of
/// literal pairs from the G.711 tables anchor both directions so a shared
/// transcription error cannot pass.
#[cfg(test)]
mod reference_tests {
    use super::*;

    // ---- ITU-T G.191 g711.c, ulaw_compress ---------------------------------
    // `short` arithmetic in the original; i32 here holds the same values.
    fn g191_ulaw_compress(linbuf: i16) -> u8 {
        let linbuf = i32::from(linbuf);
        let mut absno = if linbuf < 0 {
            ((!linbuf) >> 2) + 33
        } else {
            (linbuf >> 2) + 33
        };
        if absno > 0x1FFF {
            absno = 0x1FFF;
        }
        let mut i = absno >> 6;
        let mut segno = 1;
        while i != 0 {
            segno += 1;
            i >>= 1;
        }
        let high_nibble = 0x0008 - segno;
        let mut low_nibble = (absno >> segno) & 0x000F;
        low_nibble = 0x000F - low_nibble;
        let mut logbuf = (high_nibble << 4) | low_nibble;
        if linbuf >= 0 {
            logbuf |= 0x0080;
        }
        logbuf as u8
    }

    // ---- ITU-T G.191 g711.c, ulaw_expand ----------------------------------
    fn g191_ulaw_expand(logbuf: u8) -> i16 {
        let logbuf = i32::from(logbuf);
        let sign = if logbuf < 0x0080 { -1 } else { 1 };
        let mut mantissa = !logbuf;
        let exponent = (mantissa >> 4) & 0x0007;
        let segment = exponent + 1;
        mantissa &= 0x000F;
        let step = 4 << segment;
        (sign * ((0x0080 << exponent) + step * mantissa + step / 2 - 4 * 33)) as i16
    }

    // ---- ITU-T G.191 g711.c, alaw_compress ---------------------------------
    fn g191_alaw_compress(linbuf: i16) -> u8 {
        let linbuf = i32::from(linbuf);
        let mut ix = if linbuf < 0 {
            (!linbuf) >> 4
        } else {
            linbuf >> 4
        };
        if ix > 15 {
            let mut iexp = 1;
            while ix > 16 + 15 {
                ix >>= 1;
                iexp += 1;
            }
            ix -= 16;
            ix += iexp << 4;
        }
        if linbuf >= 0 {
            ix |= 0x0080;
        }
        (ix ^ 0x0055) as u8
    }

    // ---- ITU-T G.191 g711.c, alaw_expand -----------------------------------
    fn g191_alaw_expand(logbuf: u8) -> i16 {
        let logbuf = i32::from(logbuf);
        let mut ix = logbuf ^ 0x0055;
        ix &= 0x007F;
        let iexp = ix >> 4;
        let mut mant = ix & 0x000F;
        if iexp > 0 {
            mant += 16;
        }
        mant = (mant << 4) + 0x0008;
        if iexp > 1 {
            mant <<= iexp - 1;
        }
        (if logbuf > 127 { mant } else { -mant }) as i16
    }

    fn disagreements(name: &str, f: impl Fn(i16) -> u8, g: impl Fn(i16) -> u8) {
        let bad: Vec<String> = (i16::MIN..=i16::MAX)
            .filter(|&s| f(s) != g(s))
            .map(|s| format!("sample {s}: crate {:#04x} G.191 {:#04x}", f(s), g(s)))
            .collect();
        assert!(
            bad.is_empty(),
            "{name}: {} of 65536 disagree; first 20: {:#?}",
            bad.len(),
            &bad[..bad.len().min(20)]
        );
    }

    #[test]
    fn ulaw_encoder_matches_g191_for_every_i16() {
        disagreements("ulaw", linear_to_ulaw, g191_ulaw_compress);
    }

    #[test]
    fn alaw_encoder_matches_g191_for_every_i16() {
        disagreements("alaw", linear_to_alaw, g191_alaw_compress);
    }

    #[test]
    fn decoders_match_g191_for_every_byte() {
        for b in 0..=u8::MAX {
            assert_eq!(ulaw_to_linear(b), g191_ulaw_expand(b), "ulaw {b:#04x}");
            assert_eq!(alaw_to_linear(b), g191_alaw_expand(b), "alaw {b:#04x}");
        }
    }

    /// Every code's decoded value encodes back to the same code, except
    /// μ-law negative zero (`0x7F`), which decodes to 0 and re-encodes as
    /// positive zero (`0xFF`).
    #[test]
    fn decoded_values_round_trip_through_the_encoders() {
        for b in 0..=u8::MAX {
            assert_eq!(linear_to_alaw(alaw_to_linear(b)), b, "alaw {b:#04x}");
            let expected = if b == 0x7F { 0xFF } else { b };
            assert_eq!(linear_to_ulaw(ulaw_to_linear(b)), expected, "ulaw {b:#04x}");
        }
    }

    /// Literal anchors from the G.711 tables (Table 2a/2b): full scale,
    /// silence and the first steps, in both directions.
    #[test]
    fn known_ulaw_pairs() {
        let decode: [(u8, i16); 6] = [
            (0xFF, 0),       // positive zero: silence
            (0x7F, 0),       // negative zero
            (0x00, -32_124), // most negative
            (0x80, 32_124),  // most positive
            (0xFE, 8),       // first positive step
            (0x7E, -8),      // first negative step
        ];
        for (byte, pcm) in decode {
            assert_eq!(ulaw_to_linear(byte), pcm, "decode {byte:#04x}");
        }
        let encode: [(i16, u8); 7] = [
            (0, 0xFF),
            (32_767, 0x80),
            (-32_768, 0x00),
            (8, 0xFE),
            (-8, 0x7E),
            (-1, 0x7F), // one's complement: the smallest negative is negative zero
            (-4, 0x7F), // ~(-4) >> 2 = 0: still negative zero; -8 is the first step
        ];
        for (pcm, byte) in encode {
            assert_eq!(linear_to_ulaw(pcm), byte, "encode {pcm}");
        }
    }

    #[test]
    fn known_alaw_pairs() {
        let decode: [(u8, i16); 6] = [
            (0xD5, 8), // A-law silence byte
            (0x55, -8),
            (0xAA, 32_256),  // largest positive
            (0x2A, -32_256), // largest negative
            (0xD4, 24),
            (0x54, -24),
        ];
        for (byte, pcm) in decode {
            assert_eq!(alaw_to_linear(byte), pcm, "decode {byte:#04x}");
        }
        let encode: [(i16, u8); 5] = [
            (0, 0xD5),
            (32_767, 0xAA),
            (-32_768, 0x2A),
            (8, 0xD5),
            (-8, 0x55),
        ];
        for (pcm, byte) in encode {
            assert_eq!(linear_to_alaw(pcm), byte, "encode {pcm}");
        }
    }
}
