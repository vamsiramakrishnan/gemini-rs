//! Lock-free audio buffers for the hot path.
//!
//! - [`SpscRing`]: wait-free single-producer single-consumer ring for audio
//!   streaming, split into a [`SpscProducer`] and an [`SpscConsumer`].
//! - [`AudioJitterBuffer`]: adaptive jitter buffer for smooth playback of
//!   network audio.

pub mod convert;
pub mod jitter;

pub use convert::{bytes_to_i16, i16_to_bytes, into_shared};
pub use jitter::{AudioJitterBuffer, BufferState, JitterConfig};

/// Wait-free single-producer single-consumer ring buffer for audio samples.
///
/// [`SpscRing::channel`] hands back the two halves, the way `mpsc::channel` does. Each half is `Send` but not
/// `Sync`, so the "exactly one producer, exactly one consumer" rule that a
/// ring like this depends on is enforced by the type system instead of by a
/// comment: the producer moves to the capture thread, the consumer to the
/// playback thread, and neither can be shared.
///
/// Backed by [`rtrb`] (the ring used across the Rust audio ecosystem):
/// no allocation after construction, no locks, no `unsafe` in this crate.
///
/// ```
/// use gemini_genai_rs::buffer::SpscRing;
///
/// let (mut tx, mut rx) = SpscRing::<i16>::channel(1024);
/// assert_eq!(tx.write(&[1, 2, 3]), 3);
/// let mut out = [0i16; 8];
/// assert_eq!(rx.read(&mut out), 3);
/// assert_eq!(&out[..3], &[1, 2, 3]);
/// ```
#[derive(Debug)]
pub struct SpscRing<T>(std::marker::PhantomData<T>);

impl<T: Copy> SpscRing<T> {
    /// Create a ring holding exactly `capacity` samples and split it into its
    /// producer and consumer halves.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is 0.
    pub fn channel(capacity: usize) -> (SpscProducer<T>, SpscConsumer<T>) {
        assert!(capacity > 0, "ring capacity must be > 0");
        let (producer, consumer) = rtrb::RingBuffer::new(capacity);
        (SpscProducer(producer), SpscConsumer(consumer))
    }
}

/// The writing half of an [`SpscRing`].
pub struct SpscProducer<T>(rtrb::Producer<T>);

/// The reading half of an [`SpscRing`].
pub struct SpscConsumer<T>(rtrb::Consumer<T>);

impl<T: Copy> SpscProducer<T> {
    /// Write as many samples as fit; returns how many were written.
    ///
    /// A short write means the consumer is behind — the caller decides
    /// whether to retry, drop, or block.
    pub fn write(&mut self, data: &[T]) -> usize {
        let (written, _remaining) = self.0.push_partial_slice(data);
        written.len()
    }

    /// Number of samples that can be written right now.
    pub fn available(&self) -> usize {
        self.0.slots()
    }

    /// Whether a write of even one sample would fail right now.
    pub fn is_full(&self) -> bool {
        self.0.is_full()
    }

    /// Total capacity in samples.
    pub fn capacity(&self) -> usize {
        self.0.buffer().capacity()
    }

    /// Whether the consumer half has been dropped: nothing written will ever
    /// be read.
    pub fn is_abandoned(&self) -> bool {
        self.0.is_abandoned()
    }
}

impl<T: Copy> SpscConsumer<T> {
    /// Read up to `out.len()` samples; returns how many were read.
    pub fn read(&mut self, out: &mut [T]) -> usize {
        let (filled, _unfilled) = self.0.pop_partial_slice(out);
        filled.len()
    }

    /// Number of samples waiting to be read.
    pub fn len(&self) -> usize {
        self.0.slots()
    }

    /// Whether nothing is waiting to be read.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Total capacity in samples.
    pub fn capacity(&self) -> usize {
        self.0.buffer().capacity()
    }

    /// Discard everything buffered without reading it — the barge-in flush.
    pub fn clear(&mut self) {
        let n = self.0.slots();
        if let Ok(chunk) = self.0.read_chunk(n) {
            chunk.commit_all();
        }
    }

    /// Whether the producer half has been dropped: once drained, nothing more
    /// will arrive.
    pub fn is_abandoned(&self) -> bool {
        self.0.is_abandoned()
    }
}

impl<T> std::fmt::Debug for SpscProducer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpscProducer")
            .field("free", &self.0.slots())
            .field("capacity", &self.0.buffer().capacity())
            .finish()
    }
}

impl<T> std::fmt::Debug for SpscConsumer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpscConsumer")
            .field("len", &self.0.slots())
            .field("capacity", &self.0.buffer().capacity())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_is_exact() {
        let (tx, rx) = SpscRing::<i16>::channel(100);
        assert_eq!(tx.capacity(), 100);
        assert_eq!(rx.capacity(), 100);
        assert!(rx.is_empty());
        assert_eq!(tx.available(), 100);
    }

    #[test]
    fn write_and_read() {
        let (mut tx, mut rx) = SpscRing::<i16>::channel(16);
        assert_eq!(tx.write(&[1i16, 2, 3, 4, 5]), 5);
        assert_eq!(rx.len(), 5);
        let mut out = [0i16; 5];
        assert_eq!(rx.read(&mut out), 5);
        assert_eq!(out, [1, 2, 3, 4, 5]);
        assert!(rx.is_empty());
    }

    #[test]
    fn wraparound() {
        let (mut tx, mut rx) = SpscRing::<i16>::channel(8);
        tx.write(&[1i16, 2, 3, 4, 5, 6]);
        let mut out = [0i16; 4];
        rx.read(&mut out);
        assert_eq!(out, [1, 2, 3, 4]);
        assert_eq!(tx.write(&[7i16, 8, 9, 10, 11, 12]), 6);
        let mut out2 = [0i16; 8];
        assert_eq!(rx.read(&mut out2), 8);
        assert_eq!(out2, [5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn overflow_returns_partial() {
        let (mut tx, _rx) = SpscRing::<i16>::channel(4);
        assert_eq!(tx.write(&[1i16, 2, 3, 4, 5, 6]), 4);
        assert!(tx.is_full());
    }

    #[test]
    fn underflow_returns_partial() {
        let (mut tx, mut rx) = SpscRing::<i16>::channel(16);
        tx.write(&[1i16, 2, 3]);
        let mut out = [0i16; 10];
        assert_eq!(rx.read(&mut out), 3);
        assert_eq!(&out[..3], &[1, 2, 3]);
    }

    #[test]
    fn clear_discards_data() {
        let (mut tx, mut rx) = SpscRing::<i16>::channel(16);
        tx.write(&[1i16, 2, 3, 4, 5]);
        assert_eq!(rx.len(), 5);
        rx.clear();
        assert!(rx.is_empty());
        assert_eq!(tx.available(), 16);
    }

    #[test]
    fn abandonment_is_visible_to_the_other_half() {
        let (tx, rx) = SpscRing::<i16>::channel(4);
        assert!(!tx.is_abandoned());
        drop(rx);
        assert!(tx.is_abandoned());
    }

    #[test]
    fn halves_move_to_their_threads() {
        // Each half is `Send` (it can move to the capture / playback thread);
        // neither is `Sync`, which is what makes "one producer, one consumer"
        // a compile-time fact rather than a comment — rtrb's own impls.
        fn is_send<T: Send>() {}
        is_send::<SpscProducer<i16>>();
        is_send::<SpscConsumer<i16>>();
    }

    #[test]
    fn concurrent_write_read() {
        let (mut tx, mut rx) = SpscRing::<i16>::channel(1024);

        let writer = std::thread::spawn(move || {
            let mut total = 0usize;
            for i in 0..1000 {
                let chunk: Vec<i16> = (0..16).map(|j| (i * 16 + j) as i16).collect();
                loop {
                    let w = tx.write(&chunk[total % 16..]);
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
                let r = rx.read(&mut buf);
                total += r;
                if r == 0 {
                    std::thread::yield_now();
                }
            }
            total
        });

        writer.join().unwrap();
        assert_eq!(reader.join().unwrap(), 16000);
    }

    #[test]
    #[should_panic(expected = "capacity must be > 0")]
    fn zero_capacity_panics() {
        SpscRing::<i16>::channel(0);
    }
}
