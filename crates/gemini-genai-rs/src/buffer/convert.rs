//! Zero-copy audio sample format conversion.
//!
//! Provides utilities for converting between i16 PCM sample slices and raw byte
//! slices using [`bytemuck`] (zero-copy reinterpret casts), and for wrapping
//! owned byte vectors as shared [`Bytes`] handles for zero-copy fan-out.

use bytes::Bytes;

/// Convert a slice of i16 PCM samples to raw bytes (zero-copy via bytemuck).
pub fn i16_to_bytes(samples: &[i16]) -> &[u8] {
    bytemuck::cast_slice(samples)
}

/// Convert raw bytes to i16 PCM samples (zero-copy via bytemuck).
///
/// Returns `None` if the byte slice length is not a multiple of 2 or the
/// slice is not 2-byte aligned. Both are normal for a slice cut from a
/// network frame, and the cast uses the machine's byte order; for bytes that
/// arrive off the wire use [`pcm16_le_samples`], which has none of these
/// conditions.
pub fn bytes_to_i16(data: &[u8]) -> Option<&[i16]> {
    bytemuck::try_cast_slice(data).ok()
}

/// Decode little-endian PCM16 bytes (the Live wire format) to samples.
///
/// Works on any slice: alignment does not matter, the byte order is always
/// little-endian, and a trailing odd byte is ignored.
///
/// ```
/// # use gemini_genai_rs::buffer::convert::pcm16_le_samples;
/// assert_eq!(pcm16_le_samples(&[1, 0, 0xff, 0xff, 9]).collect::<Vec<_>>(), [1, -1]);
/// ```
pub fn pcm16_le_samples(data: &[u8]) -> impl ExactSizeIterator<Item = i16> + '_ {
    data.as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
}

/// Encode samples as little-endian PCM16 bytes (the Live wire format).
///
/// ```
/// # use gemini_genai_rs::buffer::convert::i16_to_pcm16_le;
/// assert_eq!(i16_to_pcm16_le(&[1, -1]), [1, 0, 0xff, 0xff]);
/// ```
pub fn i16_to_pcm16_le(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// Wrap raw bytes as a shared `Bytes` handle for zero-copy fan-out.
///
/// `Bytes::clone()` is O(1) — it bumps an internal `Arc` refcount instead of
/// copying the data. Use this when the same audio chunk must be broadcast to
/// multiple subscribers.
pub fn into_shared(data: Vec<u8>) -> Bytes {
    Bytes::from(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_i16_to_bytes_round_trip() {
        let samples: Vec<i16> = vec![0, 1, -1, i16::MAX, i16::MIN, 12345];
        let raw = i16_to_bytes(&samples);
        assert_eq!(raw.len(), samples.len() * 2);

        let back = bytes_to_i16(raw).expect("round-trip should succeed");
        assert_eq!(back, &samples[..]);
    }

    #[test]
    fn test_bytes_to_i16_invalid_length() {
        // Odd-length byte slice cannot be reinterpreted as &[i16]
        let odd = vec![1u8, 2, 3];
        assert!(bytes_to_i16(&odd).is_none());
    }

    #[test]
    fn pcm16_le_round_trips_and_ignores_a_trailing_odd_byte() {
        let samples = [0i16, 1, -1, i16::MAX, i16::MIN, 12345];
        let bytes = i16_to_pcm16_le(&samples);
        assert_eq!(bytes.len(), samples.len() * 2);
        assert_eq!(&bytes[..4], &[0, 0, 1, 0]);
        assert_eq!(pcm16_le_samples(&bytes).collect::<Vec<_>>(), samples);

        let mut odd = bytes;
        odd.push(7);
        assert_eq!(pcm16_le_samples(&odd).len(), samples.len());
        assert_eq!(pcm16_le_samples(&[]).len(), 0);
    }

    #[test]
    fn pcm16_le_decodes_a_misaligned_slice_the_zero_copy_cast_refuses() {
        // Two samples (1, -1), copied to whichever start is odd-addressed.
        let mut backing = [0u8; 8];
        let odd_start = usize::from((backing.as_ptr() as usize).is_multiple_of(2));
        backing[odd_start..odd_start + 4].copy_from_slice(&[1, 0, 0xff, 0xff]);
        let misaligned = &backing[odd_start..odd_start + 4];
        assert!(bytes_to_i16(misaligned).is_none());
        assert_eq!(pcm16_le_samples(misaligned).collect::<Vec<_>>(), [1, -1]);
    }

    #[test]
    fn test_shared_bytes_clone_is_cheap() {
        let original = vec![42u8; 4096];
        let ptr = original.as_ptr();
        let shared = into_shared(original);

        // Clone should share the same backing allocation
        let cloned = shared.clone();
        assert_eq!(shared.as_ptr(), cloned.as_ptr());
        // The data pointer may differ from the original Vec (Bytes may reallocate
        // during From<Vec<u8>>), but the two Bytes handles must share the same
        // underlying memory.
        let _ = ptr; // original vec was consumed
        assert_eq!(&shared[..], &cloned[..]);
        assert_eq!(shared.len(), 4096);
    }
}
