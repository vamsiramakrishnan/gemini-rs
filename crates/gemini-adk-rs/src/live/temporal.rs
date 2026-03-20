//! Temporal pattern detection for live sessions.
//!
//! A [`TemporalRegistry`] holds named [`TemporalPattern`]s that combine a
//! [`PatternDetector`] (the condition) with an async action (the response).
//! Detectors track time-based and count-based conditions such as sustained
//! state, event rates, consecutive turns, and consecutive tool failures.
//!
//! The registry is evaluated by the control-lane processor on each event and
//! optionally on a periodic timer (when [`TemporalRegistry::needs_timer`]
//! returns `true`).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gemini_genai_rs::session::{SessionEvent, SessionWriter};

use super::BoxFuture;
use crate::state::State;

// ── PatternDetector trait ─────────────────────────────────────────────────────

/// A detector that evaluates whether a temporal pattern has been triggered.
///
/// Implementations track internal state (timestamps, counters) to decide
/// when a pattern fires. All interior mutability is handled via atomic
/// operations or `parking_lot` locks so that `&self` suffices.
pub trait PatternDetector: Send + Sync {
    /// Evaluate whether the pattern is currently triggered.
    ///
    /// - `state`: the current agent state snapshot.
    /// - `event`: the session event that prompted this check (if any).
    /// - `now`: the current instant (passed in for testability).
    fn check(&self, state: &State, event: Option<&SessionEvent>, now: Instant) -> bool;

    /// Reset the detector's internal state (counters, timestamps, etc.).
    fn reset(&self);

    /// Whether this detector requires periodic timer checks.
    ///
    /// Detectors that depend on elapsed time (e.g. [`SustainedDetector`])
    /// should return `true` so the runtime can schedule a timer.
    fn needs_timer(&self) -> bool {
        false
    }
}

// ── TemporalPattern ───────────────────────────────────────────────────────────

/// A named temporal pattern: detector + action + cooldown.
pub struct TemporalPattern {
    /// Human-readable name for logging/debugging.
    pub name: String,
    /// The detector that decides when to fire.
    pub detector: Box<dyn PatternDetector>,
    /// The async action to execute when the pattern triggers.
    /// Receives a cloned `State` and the session writer.
    pub action: Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>,
    /// Optional minimum interval between successive firings.
    pub cooldown: Option<Duration>,
    /// Tracks when this pattern last fired (for cooldown enforcement).
    last_triggered: parking_lot::Mutex<Option<Instant>>,
}

impl TemporalPattern {
    /// Create a new temporal pattern.
    pub fn new(
        name: impl Into<String>,
        detector: Box<dyn PatternDetector>,
        action: Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync>,
        cooldown: Option<Duration>,
    ) -> Self {
        Self {
            name: name.into(),
            detector,
            action,
            cooldown,
            last_triggered: parking_lot::Mutex::new(None),
        }
    }

    /// Check whether the pattern fires and cooldown allows it.
    fn try_fire(
        &self,
        state: &State,
        event: Option<&SessionEvent>,
        writer: &Arc<dyn SessionWriter>,
        now: Instant,
    ) -> Option<BoxFuture<()>> {
        if !self.detector.check(state, event, now) {
            return None;
        }

        // Enforce cooldown.
        let mut last = self.last_triggered.lock();
        if let (Some(cooldown), Some(prev)) = (self.cooldown, *last) {
            if now.duration_since(prev) < cooldown {
                return None;
            }
        }

        *last = Some(now);

        let s = state.clone();
        let w = writer.clone();
        Some((self.action)(s, w))
    }
}

// ── TemporalRegistry ─────────────────────────────────────────────────────────

/// Registry of temporal patterns evaluated on events and/or timer ticks.
pub struct TemporalRegistry {
    patterns: Vec<TemporalPattern>,
}

impl Default for TemporalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TemporalRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            patterns: Vec::new(),
        }
    }

    /// Add a pattern to the registry.
    pub fn add(&mut self, pattern: TemporalPattern) {
        self.patterns.push(pattern);
    }

    /// Check all patterns and return the futures for those that fired.
    ///
    /// Called by the control-lane processor on each event and optionally on
    /// a periodic timer tick.
    pub fn check_all(
        &self,
        state: &State,
        event: Option<&SessionEvent>,
        writer: &Arc<dyn SessionWriter>,
    ) -> Vec<BoxFuture<()>> {
        let now = Instant::now();
        self.patterns
            .iter()
            .filter_map(|p| p.try_fire(state, event, writer, now))
            .collect()
    }

    /// Returns `true` if any registered pattern's detector needs periodic
    /// timer checks (i.e. its [`PatternDetector::needs_timer`] returns `true`).
    pub fn needs_timer(&self) -> bool {
        self.patterns.iter().any(|p| p.detector.needs_timer())
    }
}

// ── SustainedDetector ─────────────────────────────────────────────────────────

/// Fires when a state-based condition remains true for at least `duration`.
///
/// On each `check()`:
/// - If the condition is true and `became_true_at` is `None`, record `now`.
/// - If the condition is true and `became_true_at` is `Some(t)`, return
///   `true` when `now - t >= duration`.
/// - If the condition is false, reset `became_true_at` to `None`.
///
/// This detector **needs periodic timer checks** because it depends on
/// elapsed wall-clock time.
pub struct SustainedDetector {
    condition: Arc<dyn Fn(&State) -> bool + Send + Sync>,
    duration: Duration,
    became_true_at: parking_lot::Mutex<Option<Instant>>,
}

impl SustainedDetector {
    /// Create a new sustained detector.
    ///
    /// - `condition`: evaluated against the current state.
    /// - `duration`: how long the condition must remain true before firing.
    pub fn new(condition: Arc<dyn Fn(&State) -> bool + Send + Sync>, duration: Duration) -> Self {
        Self {
            condition,
            duration,
            became_true_at: parking_lot::Mutex::new(None),
        }
    }
}

impl PatternDetector for SustainedDetector {
    fn check(&self, state: &State, _event: Option<&SessionEvent>, now: Instant) -> bool {
        if (self.condition)(state) {
            let mut guard = self.became_true_at.lock();
            match *guard {
                None => {
                    *guard = Some(now);
                    false
                }
                Some(t) => now.duration_since(t) >= self.duration,
            }
        } else {
            *self.became_true_at.lock() = None;
            false
        }
    }

    fn reset(&self) {
        *self.became_true_at.lock() = None;
    }

    fn needs_timer(&self) -> bool {
        true
    }
}

// ── RateDetector ──────────────────────────────────────────────────────────────

/// Fires when at least `count` matching events occur within `window`.
///
/// On each `check()`:
/// - If `event` is `Some` and the filter accepts it, push the current
///   timestamp.
/// - Expire timestamps older than `window`.
/// - Return `true` if the remaining count >= threshold.
pub struct RateDetector {
    filter: Arc<dyn Fn(&SessionEvent) -> bool + Send + Sync>,
    count: u32,
    window: Duration,
    timestamps: parking_lot::Mutex<VecDeque<Instant>>,
}

impl RateDetector {
    /// Create a new rate detector.
    ///
    /// - `filter`: predicate to select which events count.
    /// - `count`: number of matching events required.
    /// - `window`: sliding time window.
    pub fn new(
        filter: Arc<dyn Fn(&SessionEvent) -> bool + Send + Sync>,
        count: u32,
        window: Duration,
    ) -> Self {
        Self {
            filter,
            count,
            window,
            timestamps: parking_lot::Mutex::new(VecDeque::new()),
        }
    }
}

impl PatternDetector for RateDetector {
    fn check(&self, _state: &State, event: Option<&SessionEvent>, now: Instant) -> bool {
        let mut ts = self.timestamps.lock();

        // Record matching event.
        if let Some(evt) = event {
            if (self.filter)(evt) {
                ts.push_back(now);
            }
        }

        // Expire old timestamps.
        while let Some(&front) = ts.front() {
            if now.duration_since(front) > self.window {
                ts.pop_front();
            } else {
                break;
            }
        }

        ts.len() as u32 >= self.count
    }

    fn reset(&self) {
        self.timestamps.lock().clear();
    }

    // RateDetector does not need timer — it is event-driven.
}

// ── TurnCountDetector ─────────────────────────────────────────────────────────

/// Fires when a state-based condition is true for `required` consecutive
/// evaluations (typically one evaluation per turn).
///
/// The caller decides when to invoke `check()` — usually on `TurnComplete`
/// events.
pub struct TurnCountDetector {
    condition: Arc<dyn Fn(&State) -> bool + Send + Sync>,
    required: u32,
    consecutive: AtomicU32,
}

impl TurnCountDetector {
    /// Create a new turn-count detector.
    ///
    /// - `condition`: evaluated against the current state each turn.
    /// - `required`: number of consecutive true results before firing.
    pub fn new(condition: Arc<dyn Fn(&State) -> bool + Send + Sync>, required: u32) -> Self {
        Self {
            condition,
            required,
            consecutive: AtomicU32::new(0),
        }
    }
}

impl PatternDetector for TurnCountDetector {
    fn check(&self, state: &State, _event: Option<&SessionEvent>, _now: Instant) -> bool {
        if (self.condition)(state) {
            let prev = self.consecutive.fetch_add(1, Ordering::SeqCst);
            prev + 1 >= self.required
        } else {
            self.consecutive.store(0, Ordering::SeqCst);
            false
        }
    }

    fn reset(&self) {
        self.consecutive.store(0, Ordering::SeqCst);
    }
}

// ── ConsecutiveFailureDetector ────────────────────────────────────────────────

/// Fires when a named tool has failed `threshold` consecutive times.
///
/// Uses a state-key convention: if `bg:{tool_name}_failed` is `true` the
/// tool is considered to have failed; if `false` (or absent) the streak
/// resets.
pub struct ConsecutiveFailureDetector {
    tool_name: String,
    threshold: u32,
    consecutive: AtomicU32,
}

impl ConsecutiveFailureDetector {
    /// Create a new consecutive-failure detector.
    ///
    /// - `tool_name`: the tool whose failures are tracked.
    /// - `threshold`: how many consecutive failures before firing.
    pub fn new(tool_name: impl Into<String>, threshold: u32) -> Self {
        Self {
            tool_name: tool_name.into(),
            threshold,
            consecutive: AtomicU32::new(0),
        }
    }
}

impl PatternDetector for ConsecutiveFailureDetector {
    fn check(&self, state: &State, _event: Option<&SessionEvent>, _now: Instant) -> bool {
        let key = format!("bg:{}_failed", self.tool_name);
        let failed: bool = state.get(&key).unwrap_or(false);

        if failed {
            let prev = self.consecutive.fetch_add(1, Ordering::SeqCst);
            prev + 1 >= self.threshold
        } else {
            self.consecutive.store(0, Ordering::SeqCst);
            false
        }
    }

    fn reset(&self) {
        self.consecutive.store(0, Ordering::SeqCst);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Dummy SessionWriter for tests — all methods return Ok.
    struct MockWriter;

    #[async_trait::async_trait]
    impl SessionWriter for MockWriter {
        async fn send_audio(
            &self,
            _: Vec<u8>,
        ) -> Result<(), gemini_genai_rs::session::SessionError> {
            Ok(())
        }
        async fn send_text(&self, _: String) -> Result<(), gemini_genai_rs::session::SessionError> {
            Ok(())
        }
        async fn send_tool_response(
            &self,
            _: Vec<gemini_genai_rs::protocol::FunctionResponse>,
        ) -> Result<(), gemini_genai_rs::session::SessionError> {
            Ok(())
        }
        async fn send_client_content(
            &self,
            _: Vec<gemini_genai_rs::protocol::Content>,
            _: bool,
        ) -> Result<(), gemini_genai_rs::session::SessionError> {
            Ok(())
        }
        async fn send_video(
            &self,
            _: Vec<u8>,
        ) -> Result<(), gemini_genai_rs::session::SessionError> {
            Ok(())
        }
        async fn update_instruction(
            &self,
            _: String,
        ) -> Result<(), gemini_genai_rs::session::SessionError> {
            Ok(())
        }
        async fn signal_activity_start(
            &self,
        ) -> Result<(), gemini_genai_rs::session::SessionError> {
            Ok(())
        }
        async fn signal_activity_end(&self) -> Result<(), gemini_genai_rs::session::SessionError> {
            Ok(())
        }
        async fn disconnect(&self) -> Result<(), gemini_genai_rs::session::SessionError> {
            Ok(())
        }
    }

    fn mock_writer() -> Arc<dyn SessionWriter> {
        Arc::new(MockWriter)
    }

    /// Helper: action that increments a shared counter.
    fn counting_action(
        counter: Arc<AtomicU32>,
    ) -> Arc<dyn Fn(State, Arc<dyn SessionWriter>) -> BoxFuture<()> + Send + Sync> {
        Arc::new(move |_state, _writer| {
            let c = counter.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
            })
        })
    }

    // ── 1. SustainedDetector fires after duration elapses ─────────────────

    #[test]
    fn sustained_fires_after_duration() {
        let state = State::new();
        state.set("hot", true);

        let detector = SustainedDetector::new(
            Arc::new(|s: &State| s.get::<bool>("hot").unwrap_or(false)),
            Duration::from_secs(5),
        );

        let t0 = Instant::now();

        // First check: records the start time, does not fire yet.
        assert!(!detector.check(&state, None, t0));

        // 3 seconds later: not yet.
        assert!(!detector.check(&state, None, t0 + Duration::from_secs(3)));

        // 5 seconds later: fires.
        assert!(detector.check(&state, None, t0 + Duration::from_secs(5)));

        // Still fires on subsequent checks while condition holds.
        assert!(detector.check(&state, None, t0 + Duration::from_secs(6)));
    }

    // ── 2. SustainedDetector resets when condition becomes false ───────────

    #[test]
    fn sustained_resets_on_false() {
        let state = State::new();
        state.set("hot", true);

        let detector = SustainedDetector::new(
            Arc::new(|s: &State| s.get::<bool>("hot").unwrap_or(false)),
            Duration::from_secs(5),
        );

        let t0 = Instant::now();

        // Start tracking.
        assert!(!detector.check(&state, None, t0));

        // Condition becomes false at t0+2s — resets internal timer.
        state.set("hot", false);
        assert!(!detector.check(&state, None, t0 + Duration::from_secs(2)));

        // Condition becomes true again at t0+3s — starts fresh.
        state.set("hot", true);
        assert!(!detector.check(&state, None, t0 + Duration::from_secs(3)));

        // t0+7s: only 4s since re-start at t0+3s — not enough.
        assert!(!detector.check(&state, None, t0 + Duration::from_secs(7)));

        // t0+8s: 5s since t0+3s — fires.
        assert!(detector.check(&state, None, t0 + Duration::from_secs(8)));
    }

    // ── 3. SustainedDetector reset() clears state ─────────────────────────

    #[test]
    fn sustained_reset_clears_state() {
        let state = State::new();
        state.set("hot", true);

        let detector = SustainedDetector::new(
            Arc::new(|s: &State| s.get::<bool>("hot").unwrap_or(false)),
            Duration::from_secs(5),
        );

        let t0 = Instant::now();

        // Start tracking.
        assert!(!detector.check(&state, None, t0));

        // Explicit reset.
        detector.reset();

        // Must start tracking from scratch — 5s from the new check.
        assert!(!detector.check(&state, None, t0 + Duration::from_secs(4)));
        assert!(detector.check(&state, None, t0 + Duration::from_secs(9)));
    }

    // ── 4. RateDetector fires when count reached in window ────────────────

    #[test]
    fn rate_fires_when_count_reached() {
        let state = State::new();
        let detector = RateDetector::new(
            Arc::new(|evt: &SessionEvent| matches!(evt, SessionEvent::TurnComplete)),
            3,
            Duration::from_secs(10),
        );

        let t0 = Instant::now();
        let event = SessionEvent::TurnComplete;

        assert!(!detector.check(&state, Some(&event), t0));
        assert!(!detector.check(&state, Some(&event), t0 + Duration::from_secs(1)));
        // Third event: fires.
        assert!(detector.check(&state, Some(&event), t0 + Duration::from_secs(2)));
    }

    // ── 5. RateDetector does not fire when events outside window ──────────

    #[test]
    fn rate_does_not_fire_when_events_outside_window() {
        let state = State::new();
        let detector = RateDetector::new(
            Arc::new(|evt: &SessionEvent| matches!(evt, SessionEvent::TurnComplete)),
            3,
            Duration::from_secs(5),
        );

        let t0 = Instant::now();
        let event = SessionEvent::TurnComplete;

        // Two events at t0.
        assert!(!detector.check(&state, Some(&event), t0));
        assert!(!detector.check(&state, Some(&event), t0 + Duration::from_secs(1)));

        // Third event at t0+10s: first two have expired.
        assert!(!detector.check(&state, Some(&event), t0 + Duration::from_secs(10)));
    }

    // ── 6. RateDetector with filter that rejects events ───────────────────

    #[test]
    fn rate_filter_rejects_events() {
        let state = State::new();
        let detector = RateDetector::new(
            Arc::new(|evt: &SessionEvent| matches!(evt, SessionEvent::TurnComplete)),
            2,
            Duration::from_secs(10),
        );

        let t0 = Instant::now();

        // These events don't match the filter.
        let text_event = SessionEvent::TextDelta("hello".to_string());
        assert!(!detector.check(&state, Some(&text_event), t0));
        assert!(!detector.check(&state, Some(&text_event), t0 + Duration::from_secs(1)));
        assert!(!detector.check(&state, Some(&text_event), t0 + Duration::from_secs(2)));

        // Still at 0 matching events — no fire.
        assert!(!detector.check(&state, None, t0 + Duration::from_secs(3)));
    }

    // ── 7. TurnCountDetector fires after N consecutive true ───────────────

    #[test]
    fn turn_count_fires_after_n_consecutive() {
        let state = State::new();
        state.set("confused", true);

        let detector = TurnCountDetector::new(
            Arc::new(|s: &State| s.get::<bool>("confused").unwrap_or(false)),
            3,
        );

        let t0 = Instant::now();

        assert!(!detector.check(&state, None, t0));
        assert!(!detector.check(&state, None, t0));
        // Third consecutive true: fires.
        assert!(detector.check(&state, None, t0));
    }

    // ── 8. TurnCountDetector resets on false ──────────────────────────────

    #[test]
    fn turn_count_resets_on_false() {
        let state = State::new();
        state.set("confused", true);

        let detector = TurnCountDetector::new(
            Arc::new(|s: &State| s.get::<bool>("confused").unwrap_or(false)),
            3,
        );

        let t0 = Instant::now();

        assert!(!detector.check(&state, None, t0));
        assert!(!detector.check(&state, None, t0));

        // Condition becomes false — resets counter.
        state.set("confused", false);
        assert!(!detector.check(&state, None, t0));

        // Start again — need 3 more consecutive trues.
        state.set("confused", true);
        assert!(!detector.check(&state, None, t0));
        assert!(!detector.check(&state, None, t0));
        assert!(detector.check(&state, None, t0));
    }

    // ── 9. ConsecutiveFailureDetector fires after threshold ───────────────

    #[test]
    fn consecutive_failure_fires_after_threshold() {
        let state = State::new();
        state.set("bg:search_failed", true);

        let detector = ConsecutiveFailureDetector::new("search", 3);

        let t0 = Instant::now();

        assert!(!detector.check(&state, None, t0));
        assert!(!detector.check(&state, None, t0));
        // Third consecutive failure: fires.
        assert!(detector.check(&state, None, t0));
    }

    // ── 10. ConsecutiveFailureDetector resets on success ──────────────────

    #[test]
    fn consecutive_failure_resets_on_success() {
        let state = State::new();
        state.set("bg:search_failed", true);

        let detector = ConsecutiveFailureDetector::new("search", 3);

        let t0 = Instant::now();

        assert!(!detector.check(&state, None, t0));
        assert!(!detector.check(&state, None, t0));

        // Tool succeeds — reset.
        state.set("bg:search_failed", false);
        assert!(!detector.check(&state, None, t0));

        // Must accumulate again from 0.
        state.set("bg:search_failed", true);
        assert!(!detector.check(&state, None, t0));
        assert!(!detector.check(&state, None, t0));
        assert!(detector.check(&state, None, t0));
    }

    // ── 11. TemporalPattern cooldown prevents rapid re-firing ─────────────

    #[tokio::test]
    async fn pattern_cooldown_prevents_rapid_refiring() {
        let counter = Arc::new(AtomicU32::new(0));
        let state = State::new();
        state.set("active", true);
        let writer = mock_writer();

        let pattern = TemporalPattern::new(
            "test-cooldown",
            Box::new(SustainedDetector::new(
                Arc::new(|s: &State| s.get::<bool>("active").unwrap_or(false)),
                Duration::from_secs(0), // fires immediately once became_true_at is set
            )),
            counting_action(counter.clone()),
            Some(Duration::from_secs(10)), // 10s cooldown
        );

        let t0 = Instant::now();

        // First check: sets became_true_at but doesn't fire (duration=0, but
        // the first check just records the start time).
        assert!(pattern.try_fire(&state, None, &writer, t0).is_none());

        // Second check: fires (condition true + duration=0 elapsed).
        let fut = pattern.try_fire(&state, None, &writer, t0 + Duration::from_millis(1));
        assert!(fut.is_some());
        fut.unwrap().await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // Immediate re-check: cooldown blocks.
        assert!(pattern
            .try_fire(&state, None, &writer, t0 + Duration::from_millis(2))
            .is_none());

        // After cooldown: fires again.
        let fut = pattern.try_fire(&state, None, &writer, t0 + Duration::from_secs(11));
        assert!(fut.is_some());
        fut.unwrap().await;
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    // ── 12. TemporalRegistry check_all returns actions ────────────────────

    #[tokio::test]
    async fn registry_check_all_returns_actions() {
        let counter = Arc::new(AtomicU32::new(0));
        let state = State::new();
        state.set("confused", true);
        let writer = mock_writer();

        let mut registry = TemporalRegistry::new();

        // TurnCountDetector with required=1 — fires on first true check.
        registry.add(TemporalPattern::new(
            "confusion",
            Box::new(TurnCountDetector::new(
                Arc::new(|s: &State| s.get::<bool>("confused").unwrap_or(false)),
                1,
            )),
            counting_action(counter.clone()),
            None,
        ));

        let actions = registry.check_all(&state, None, &writer);
        assert_eq!(actions.len(), 1);

        for fut in actions {
            fut.await;
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // ── 13. needs_timer returns true when SustainedDetector is registered ─

    #[test]
    fn needs_timer_true_with_sustained_detector() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = TemporalRegistry::new();

        registry.add(TemporalPattern::new(
            "sustained",
            Box::new(SustainedDetector::new(
                Arc::new(|_: &State| true),
                Duration::from_secs(5),
            )),
            counting_action(counter),
            None,
        ));

        assert!(registry.needs_timer());
    }

    // ── 14. needs_timer returns false when no SustainedDetector ───────────

    #[test]
    fn needs_timer_false_without_sustained_detector() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = TemporalRegistry::new();

        registry.add(TemporalPattern::new(
            "turn-count",
            Box::new(TurnCountDetector::new(Arc::new(|_: &State| true), 3)),
            counting_action(counter.clone()),
            None,
        ));

        registry.add(TemporalPattern::new(
            "rate",
            Box::new(RateDetector::new(
                Arc::new(|_: &SessionEvent| true),
                5,
                Duration::from_secs(10),
            )),
            counting_action(counter),
            None,
        ));

        assert!(!registry.needs_timer());
    }

    // ── Additional: Default creates empty registry ────────────────────────

    #[test]
    fn default_creates_empty_registry() {
        let registry = TemporalRegistry::default();
        assert!(!registry.needs_timer());
    }

    // ── Additional: RateDetector reset clears timestamps ──────────────────

    #[test]
    fn rate_reset_clears_timestamps() {
        let state = State::new();
        let detector = RateDetector::new(
            Arc::new(|evt: &SessionEvent| matches!(evt, SessionEvent::TurnComplete)),
            2,
            Duration::from_secs(10),
        );

        let t0 = Instant::now();
        let event = SessionEvent::TurnComplete;

        assert!(!detector.check(&state, Some(&event), t0));
        detector.reset();
        // After reset, first event should not be enough.
        assert!(!detector.check(&state, Some(&event), t0 + Duration::from_secs(1)));
        // Second event after reset fires.
        assert!(detector.check(&state, Some(&event), t0 + Duration::from_secs(2)));
    }

    // ── Additional: TurnCountDetector reset clears counter ────────────────

    #[test]
    fn turn_count_reset_clears_counter() {
        let state = State::new();
        state.set("confused", true);

        let detector = TurnCountDetector::new(
            Arc::new(|s: &State| s.get::<bool>("confused").unwrap_or(false)),
            3,
        );

        let t0 = Instant::now();

        assert!(!detector.check(&state, None, t0));
        assert!(!detector.check(&state, None, t0));
        // Two consecutive trues accumulated.

        detector.reset();

        // After reset, need 3 more.
        assert!(!detector.check(&state, None, t0));
        assert!(!detector.check(&state, None, t0));
        assert!(detector.check(&state, None, t0));
    }

    // ── Additional: ConsecutiveFailureDetector reset clears counter ───────

    #[test]
    fn consecutive_failure_reset_clears_counter() {
        let state = State::new();
        state.set("bg:search_failed", true);

        let detector = ConsecutiveFailureDetector::new("search", 3);
        let t0 = Instant::now();

        assert!(!detector.check(&state, None, t0));
        assert!(!detector.check(&state, None, t0));

        detector.reset();

        assert!(!detector.check(&state, None, t0));
        assert!(!detector.check(&state, None, t0));
        assert!(detector.check(&state, None, t0));
    }

    // ── Additional: SustainedDetector needs_timer is true ─────────────────

    #[test]
    fn sustained_detector_needs_timer() {
        let detector = SustainedDetector::new(Arc::new(|_: &State| true), Duration::from_secs(5));
        assert!(detector.needs_timer());
    }

    // ── Additional: RateDetector needs_timer is false ─────────────────────

    #[test]
    fn rate_detector_does_not_need_timer() {
        let detector = RateDetector::new(
            Arc::new(|_: &SessionEvent| true),
            5,
            Duration::from_secs(10),
        );
        assert!(!detector.needs_timer());
    }

    // ── Additional: TurnCountDetector needs_timer is false ────────────────

    #[test]
    fn turn_count_detector_does_not_need_timer() {
        let detector = TurnCountDetector::new(Arc::new(|_: &State| true), 3);
        assert!(!detector.needs_timer());
    }

    // ── Additional: Pattern without cooldown fires every time ─────────────

    #[tokio::test]
    async fn pattern_without_cooldown_fires_every_time() {
        let counter = Arc::new(AtomicU32::new(0));
        let state = State::new();
        state.set("active", true);
        let writer = mock_writer();

        let pattern = TemporalPattern::new(
            "no-cooldown",
            Box::new(TurnCountDetector::new(
                Arc::new(|s: &State| s.get::<bool>("active").unwrap_or(false)),
                1,
            )),
            counting_action(counter.clone()),
            None, // no cooldown
        );

        let t0 = Instant::now();

        for i in 0..5u32 {
            let fut = pattern.try_fire(&state, None, &writer, t0 + Duration::from_millis(i as u64));
            assert!(fut.is_some(), "should fire on iteration {i}");
            fut.unwrap().await;
        }

        assert_eq!(counter.load(Ordering::SeqCst), 5);
    }
}
