//! Lock-free audio buffers for the hot path.
//!
//! - [`SpscRing`]: Single-producer single-consumer ring buffer for zero-copy audio streaming.
//! - [`AudioJitterBuffer`]: Adaptive jitter buffer for smooth playback of network audio.

pub mod jitter;

pub use jitter::{AudioJitterBuffer, JitterConfig};

use std::sync::atomic::{AtomicUsize, Ordering};

/// Cache-line padding to prevent false sharing between producer and consumer.
///
/// 128 bytes covers both x86_64 (64B) and Apple Silicon (128B) cache lines.
#[repr(align(128))]
struct CachePad<T>(T);

/// Lock-free single-producer single-consumer ring buffer.
///
/// The hot path for audio data. No heap allocation after initialization.
/// Uses atomic head/tail pointers with cache-line padding to prevent false sharing.
///
/// # Performance
///
/// - Wait-free on the fast path (atomic store/load only)
/// - Power-of-two capacity for bitwise modulo (single-cycle AND vs multi-cycle DIV)
/// - Bounded memory, no allocation after init
///
/// # Safety
///
/// This buffer is safe for concurrent use by exactly one producer and one consumer.
/// Multiple producers or multiple consumers will cause data races.
pub struct SpscRing<T: Copy + Default> {
    buf: Box<[T]>,
    cap_mask: usize,
    head: CachePad<AtomicUsize>,
    tail: CachePad<AtomicUsize>,
}

impl<T: Copy + Default> SpscRing<T> {
    /// Create a new ring buffer with the given capacity.
    ///
    /// Capacity is rounded up to the next power of two for efficient modulo.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is 0.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "ring buffer capacity must be > 0");
        let cap = capacity.next_power_of_two();
        let buf = vec![T::default(); cap].into_boxed_slice();
        Self {
            buf,
            cap_mask: cap - 1,
            head: CachePad(AtomicUsize::new(0)),
            tail: CachePad(AtomicUsize::new(0)),
        }
    }

    /// Returns the usable capacity of the buffer.
    pub fn capacity(&self) -> usize {
        self.cap_mask + 1
    }

    /// Returns the number of items currently available to read.
    pub fn len(&self) -> usize {
        let head = self.head.0.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        head.wrapping_sub(tail)
    }

    /// Returns true if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the number of free slots available for writing.
    pub fn available(&self) -> usize {
        self.capacity() - self.len()
    }

    /// Write samples into the ring buffer.
    ///
    /// Returns the number of samples actually written (may be less than
    /// `data.len()` if the buffer is nearly full).
    ///
    /// This is the **producer** method — call from exactly one thread.
    pub fn write(&self, data: &[T]) -> usize {
        let head = self.head.0.load(Ordering::Relaxed);
        let tail = self.tail.0.load(Ordering::Acquire);

        let free = self.capacity() - head.wrapping_sub(tail);
        let to_write = data.len().min(free);

        if to_write == 0 {
            return 0;
        }

        let start = head & self.cap_mask;
        let end = start + to_write;

        if end <= self.capacity() {
            // Contiguous write
            // SAFETY: we are the sole producer and have verified space is available
            unsafe {
                let dst = self.buf.as_ptr() as *mut T;
                std::ptr::copy_nonoverlapping(data.as_ptr(), dst.add(start), to_write);
            }
        } else {
            // Wrapped write: two segments
            let first_len = self.capacity() - start;
            let second_len = to_write - first_len;
            unsafe {
                let dst = self.buf.as_ptr() as *mut T;
                std::ptr::copy_nonoverlapping(data.as_ptr(), dst.add(start), first_len);
                std::ptr::copy_nonoverlapping(data.as_ptr().add(first_len), dst, second_len);
            }
        }

        self.head
            .0
            .store(head.wrapping_add(to_write), Ordering::Release);
        to_write
    }

    /// Read samples from the ring buffer into `out`.
    ///
    /// Returns the number of samples actually read (may be less than
    /// `out.len()` if the buffer has fewer samples available).
    ///
    /// This is the **consumer** method — call from exactly one thread.
    pub fn read(&self, out: &mut [T]) -> usize {
        let tail = self.tail.0.load(Ordering::Relaxed);
        let head = self.head.0.load(Ordering::Acquire);

        let available = head.wrapping_sub(tail);
        let to_read = out.len().min(available);

        if to_read == 0 {
            return 0;
        }

        let start = tail & self.cap_mask;
        let end = start + to_read;

        if end <= self.capacity() {
            out[..to_read].copy_from_slice(&self.buf[start..start + to_read]);
        } else {
            let first_len = self.capacity() - start;
            let second_len = to_read - first_len;
            out[..first_len].copy_from_slice(&self.buf[start..]);
            out[first_len..to_read].copy_from_slice(&self.buf[..second_len]);
        }

        self.tail
            .0
            .store(tail.wrapping_add(to_read), Ordering::Release);
        to_read
    }

    /// Discard all buffered data without reading it.
    pub fn clear(&self) {
        let head = self.head.0.load(Ordering::Acquire);
        self.tail.0.store(head, Ordering::Release);
    }
}

// SAFETY: SpscRing is safe to share across threads. The atomic operations on head/tail
// provide the necessary synchronization. The invariant that exactly one producer and
// one consumer exist must be upheld by the caller.
unsafe impl<T: Copy + Default + Send> Send for SpscRing<T> {}
unsafe impl<T: Copy + Default + Send> Sync for SpscRing<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_ring() {
        let ring = SpscRing::<i16>::new(100);
        // Rounds up to 128
        assert_eq!(ring.capacity(), 128);
        assert!(ring.is_empty());
        assert_eq!(ring.available(), 128);
    }

    #[test]
    fn write_and_read() {
        let ring = SpscRing::<i16>::new(16);
        let data = [1i16, 2, 3, 4, 5];

        let written = ring.write(&data);
        assert_eq!(written, 5);
        assert_eq!(ring.len(), 5);

        let mut out = [0i16; 5];
        let read = ring.read(&mut out);
        assert_eq!(read, 5);
        assert_eq!(out, [1, 2, 3, 4, 5]);
        assert!(ring.is_empty());
    }

    #[test]
    fn wraparound() {
        let ring = SpscRing::<i16>::new(8); // capacity = 8
        let data = [1i16, 2, 3, 4, 5, 6];

        ring.write(&data);
        let mut out = [0i16; 4];
        ring.read(&mut out); // consume 4, tail = 4
        assert_eq!(out, [1, 2, 3, 4]);

        // Write more — will wrap around
        let data2 = [7i16, 8, 9, 10, 11, 12];
        let written = ring.write(&data2);
        assert_eq!(written, 6);

        let mut out2 = [0i16; 8];
        let read = ring.read(&mut out2);
        assert_eq!(read, 8);
        assert_eq!(&out2[..8], &[5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn overflow_returns_partial() {
        let ring = SpscRing::<i16>::new(4); // capacity = 4
        let data = [1i16, 2, 3, 4, 5, 6]; // too many
        let written = ring.write(&data);
        assert_eq!(written, 4);
    }

    #[test]
    fn underflow_returns_partial() {
        let ring = SpscRing::<i16>::new(16);
        ring.write(&[1i16, 2, 3]);

        let mut out = [0i16; 10];
        let read = ring.read(&mut out);
        assert_eq!(read, 3);
        assert_eq!(&out[..3], &[1, 2, 3]);
    }

    #[test]
    fn clear_discards_data() {
        let ring = SpscRing::<i16>::new(16);
        ring.write(&[1i16, 2, 3, 4, 5]);
        assert_eq!(ring.len(), 5);

        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.available(), 16);
    }

    #[test]
    fn concurrent_write_read() {
        use std::sync::Arc;

        let ring = Arc::new(SpscRing::<i16>::new(1024));
        let ring_w = ring.clone();
        let ring_r = ring.clone();

        let writer = std::thread::spawn(move || {
            let mut total = 0usize;
            for i in 0..1000 {
                let chunk: Vec<i16> = (0..16).map(|j| (i * 16 + j) as i16).collect();
                loop {
                    let w = ring_w.write(&chunk[total % 16..]);
                    total += w;
                    if total >= (i as usize + 1) * 16 {
                        break;
                    }
                    std::thread::yield_now();
                }
            }
        });

        let reader = std::thread::spawn(move || {
            let mut total = 0usize;
            let mut buf = [0i16; 64];
            while total < 16000 {
                let r = ring_r.read(&mut buf);
                total += r;
                if r == 0 {
                    std::thread::yield_now();
                }
            }
            total
        });

        writer.join().unwrap();
        let total_read = reader.join().unwrap();
        assert_eq!(total_read, 16000);
    }

    #[test]
    #[should_panic(expected = "capacity must be > 0")]
    fn zero_capacity_panics() {
        SpscRing::<i16>::new(0);
    }
}
