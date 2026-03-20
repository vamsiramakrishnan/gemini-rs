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
/// Returns `None` if the byte slice length is not a multiple of 2.
pub fn bytes_to_i16(data: &[u8]) -> Option<&[i16]> {
    bytemuck::try_cast_slice(data).ok()
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
