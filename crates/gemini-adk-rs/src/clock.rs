//! The time source the runtime reads, so that time is an input you control.
//!
//! Every decision the runtime makes from elapsed time reads a [`Clock`]:
//!
//! - temporal patterns ("sustained for 5 s");
//! - phase durations;
//! - resolver cache expiry;
//! - the `session:` timing signals (`silence_ms`, `elapsed_ms`, …);
//! - mutation-journal timestamps.
//!
//! In production that is the [`SystemClock`]. In a test or a replay it is a
//! [`ManualClock`] you advance yourself, so the same inputs give the same
//! decisions every run. [`replay_session`](crate::live::replay::replay_session)
//! drives one from the recorded frame timestamps.
//!
//! The clock travels with [`State`](crate::state::State): every clone and
//! delta view shares it, so a component that can read state can read the
//! time without another parameter.
//!
//! ```
//! use std::sync::Arc;
//! use std::time::Duration;
//! use gemini_adk_rs::clock::{Clock, ManualClock};
//! use gemini_adk_rs::State;
//!
//! let clock = Arc::new(ManualClock::new());
//! let state = State::new().with_clock(clock.clone());
//!
//! let t0 = state.clock().now();
//! clock.advance(Duration::from_secs(5));
//! assert_eq!(state.clock().now() - t0, Duration::from_secs(5));
//! ```

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// A source of monotonic and wall-clock time.
pub trait Clock: Send + Sync + fmt::Debug {
    /// The current monotonic instant. Use for durations and deadlines.
    fn now(&self) -> Instant;

    /// The current wall-clock time. Use for timestamps that leave the process.
    fn system_time(&self) -> SystemTime;

    /// Time elapsed since `earlier`, per this clock. Saturates at zero.
    fn since(&self, earlier: Instant) -> Duration {
        self.now().saturating_duration_since(earlier)
    }
}

/// A shared, dynamically dispatched clock.
pub type SharedClock = Arc<dyn Clock>;

/// The real clock: [`Instant::now`] and [`SystemTime::now`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn system_time(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// The default clock: a shared [`SystemClock`].
pub fn system_clock() -> SharedClock {
    Arc::new(SystemClock)
}

/// A clock that moves only when told to.
///
/// It starts at the moment of construction and never goes backwards:
/// [`set_elapsed`](Self::set_elapsed) to an earlier point is ignored, so
/// out-of-order inputs cannot make a duration negative.
#[derive(Debug)]
pub struct ManualClock {
    origin: Instant,
    system_origin: SystemTime,
    elapsed_nanos: AtomicU64,
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualClock {
    /// A clock whose wall time starts at the current system time.
    pub fn new() -> Self {
        Self::starting_at(SystemTime::now())
    }

    /// A clock whose wall time starts at `system_origin` (for example, the
    /// first timestamp of a recording).
    pub fn starting_at(system_origin: SystemTime) -> Self {
        Self {
            origin: Instant::now(),
            system_origin,
            elapsed_nanos: AtomicU64::new(0),
        }
    }

    /// Move the clock forward by `by`.
    pub fn advance(&self, by: Duration) {
        self.elapsed_nanos
            .fetch_add(saturating_nanos(by), Ordering::AcqRel);
    }

    /// Move the clock to `elapsed` after its origin, if that is later than
    /// where it is now.
    pub fn set_elapsed(&self, elapsed: Duration) {
        self.elapsed_nanos
            .fetch_max(saturating_nanos(elapsed), Ordering::AcqRel);
    }

    /// Time elapsed since the clock's origin.
    pub fn elapsed(&self) -> Duration {
        Duration::from_nanos(self.elapsed_nanos.load(Ordering::Acquire))
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.origin + self.elapsed()
    }

    fn system_time(&self) -> SystemTime {
        self.system_origin + self.elapsed()
    }
}

fn saturating_nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_moves_only_when_told() {
        let clock = ManualClock::starting_at(SystemTime::UNIX_EPOCH);
        let t0 = clock.now();
        assert_eq!(clock.now(), t0);

        clock.advance(Duration::from_millis(250));
        assert_eq!(clock.since(t0), Duration::from_millis(250));
        assert_eq!(
            clock.system_time(),
            SystemTime::UNIX_EPOCH + Duration::from_millis(250)
        );
    }

    #[test]
    fn manual_clock_never_goes_backwards() {
        let clock = ManualClock::new();
        clock.set_elapsed(Duration::from_secs(10));
        clock.set_elapsed(Duration::from_secs(3));
        assert_eq!(clock.elapsed(), Duration::from_secs(10));
    }

    #[test]
    fn since_saturates_for_a_future_instant() {
        let clock = ManualClock::new();
        let later = clock.now() + Duration::from_secs(1);
        assert_eq!(clock.since(later), Duration::ZERO);
    }
}
