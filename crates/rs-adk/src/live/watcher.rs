//! State change watchers with predicate-based triggering.
//!
//! A [`WatcherRegistry`] holds named [`Watcher`]s that observe specific state
//! keys and fire async actions when a [`WatchPredicate`] matches a state diff.
//!
//! The registry is evaluated by the control-lane processor after each mutation
//! cycle. It returns two sets of futures: blocking (awaited sequentially on the
//! control lane) and concurrent (spawned via `tokio::spawn`).

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value;

use super::BoxFuture;
use crate::state::State;

// ── Predicate ────────────────────────────────────────────────────────────────

/// Custom predicate function type for state change watchers.
pub type PredicateFn = Arc<dyn Fn(&Value, &Value) -> bool + Send + Sync>;

/// Condition under which a watcher fires, evaluated against (old, new) values.
pub enum WatchPredicate {
    /// Fires whenever the watched key's value changed (any diff entry).
    Changed,
    /// Fires when the new value equals the given value.
    ChangedTo(Value),
    /// Fires when the old value equals the given value.
    ChangedFrom(Value),
    /// Fires when old < threshold AND new >= threshold (both must be numeric).
    CrossedAbove(f64),
    /// Fires when old >= threshold AND new < threshold (both must be numeric).
    CrossedBelow(f64),
    /// Fires when old != true AND new == true (JSON bool).
    BecameTrue,
    /// Fires when old == true AND new != true (JSON bool).
    BecameFalse,
    /// Fires when the custom function returns true for (old, new).
    Custom(PredicateFn),
}

impl std::fmt::Debug for WatchPredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Changed => write!(f, "Changed"),
            Self::ChangedTo(v) => write!(f, "ChangedTo({v})"),
            Self::ChangedFrom(v) => write!(f, "ChangedFrom({v})"),
            Self::CrossedAbove(t) => write!(f, "CrossedAbove({t})"),
            Self::CrossedBelow(t) => write!(f, "CrossedBelow({t})"),
            Self::BecameTrue => write!(f, "BecameTrue"),
            Self::BecameFalse => write!(f, "BecameFalse"),
            Self::Custom(_) => write!(f, "Custom(<fn>)"),
        }
    }
}

impl WatchPredicate {
    /// Evaluate whether this predicate matches the given old/new value pair.
    fn matches(&self, old: &Value, new: &Value) -> bool {
        match self {
            WatchPredicate::Changed => true,
            WatchPredicate::ChangedTo(val) => new == val,
            WatchPredicate::ChangedFrom(val) => old == val,
            WatchPredicate::CrossedAbove(threshold) => {
                match (as_f64(old), as_f64(new)) {
                    (Some(o), Some(n)) => o < *threshold && n >= *threshold,
                    _ => false,
                }
            }
            WatchPredicate::CrossedBelow(threshold) => {
                match (as_f64(old), as_f64(new)) {
                    (Some(o), Some(n)) => o >= *threshold && n < *threshold,
                    _ => false,
                }
            }
            WatchPredicate::BecameTrue => {
                old != &Value::Bool(true) && new == &Value::Bool(true)
            }
            WatchPredicate::BecameFalse => {
                old == &Value::Bool(true) && new != &Value::Bool(true)
            }
            WatchPredicate::Custom(f) => f(old, new),
        }
    }
}

// ── Watcher ──────────────────────────────────────────────────────────────────

/// A single state watcher: observes one key, fires an async action when the
/// predicate matches.
pub struct Watcher {
    /// The state key to observe.
    pub key: String,
    /// The condition under which this watcher fires.
    pub predicate: WatchPredicate,
    /// Async action receiving (old_value, new_value, state).
    pub action: Arc<dyn Fn(Value, Value, State) -> BoxFuture<()> + Send + Sync>,
    /// If `true`, the processor awaits this action sequentially on the control
    /// lane. If `false`, the processor spawns it concurrently.
    pub blocking: bool,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("key", &self.key)
            .field("predicate", &self.predicate)
            .field("blocking", &self.blocking)
            .finish_non_exhaustive()
    }
}

// ── WatcherRegistry ──────────────────────────────────────────────────────────

/// Registry of state watchers, evaluated after each mutation cycle.
pub struct WatcherRegistry {
    watchers: Vec<Watcher>,
    /// Keys that any watcher observes -- used to scope snapshot/diff.
    observed_keys: HashSet<String>,
}

impl Default for WatcherRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WatcherRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            watchers: Vec::new(),
            observed_keys: HashSet::new(),
        }
    }

    /// Add a watcher to the registry.
    pub fn add(&mut self, watcher: Watcher) {
        self.observed_keys.insert(watcher.key.clone());
        self.watchers.push(watcher);
    }

    /// The set of state keys observed by at least one watcher.
    ///
    /// Used by the processor to scope `State::snapshot_values()` so only
    /// relevant keys are captured before mutations.
    pub fn observed_keys(&self) -> &HashSet<String> {
        &self.observed_keys
    }

    /// Evaluate all watchers against the given state diffs.
    ///
    /// `diffs` contains `(key, old_value, new_value)` tuples produced by
    /// `State::diff_values()`. For each diff entry, every watcher whose key
    /// matches and whose predicate fires will have its action invoked.
    ///
    /// Returns `(blocking_futures, concurrent_futures)`.
    pub fn evaluate(
        &self,
        diffs: &[(String, Value, Value)],
        state: &State,
    ) -> (Vec<BoxFuture<()>>, Vec<BoxFuture<()>>) {
        let mut blocking = Vec::new();
        let mut concurrent = Vec::new();

        for (key, old, new) in diffs {
            for watcher in &self.watchers {
                if watcher.key == *key && watcher.predicate.matches(old, new) {
                    let fut = (watcher.action)(old.clone(), new.clone(), state.clone());
                    if watcher.blocking {
                        blocking.push(fut);
                    } else {
                        concurrent.push(fut);
                    }
                }
            }
        }

        (blocking, concurrent)
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Extract an `f64` from a JSON value (only works for `Value::Number`).
fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Helper: create a watcher that increments a shared counter when fired.
    fn counting_watcher(
        key: &str,
        predicate: WatchPredicate,
        counter: Arc<AtomicU32>,
        blocking: bool,
    ) -> Watcher {
        Watcher {
            key: key.to_string(),
            predicate,
            action: Arc::new(move |_old, _new, _state| {
                let c = counter.clone();
                Box::pin(async move {
                    c.fetch_add(1, Ordering::SeqCst);
                })
            }),
            blocking,
        }
    }

    /// Helper: create a watcher that stores old+new values into state.
    fn recording_watcher(
        key: &str,
        predicate: WatchPredicate,
        blocking: bool,
    ) -> Watcher {
        Watcher {
            key: key.to_string(),
            predicate,
            action: Arc::new(|old, new, state| {
                Box::pin(async move {
                    state.set("recorded_old", old);
                    state.set("recorded_new", new);
                })
            }),
            blocking,
        }
    }

    // ── 1. Changed predicate fires on any diff ──────────────────────────

    #[tokio::test]
    async fn changed_fires_on_any_diff() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher("x", WatchPredicate::Changed, counter.clone(), false));

        let state = State::new();
        let diffs = vec![("x".to_string(), json!(1), json!(2))];

        let (blocking, concurrent) = registry.evaluate(&diffs, &state);
        assert!(blocking.is_empty());
        assert_eq!(concurrent.len(), 1);

        for fut in concurrent {
            fut.await;
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // ── 2. ChangedTo fires only when new value matches ──────────────────

    #[tokio::test]
    async fn changed_to_fires_when_new_value_matches() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher(
            "status",
            WatchPredicate::ChangedTo(json!("active")),
            counter.clone(),
            false,
        ));

        let state = State::new();
        let diffs = vec![("status".to_string(), json!("inactive"), json!("active"))];

        let (_, concurrent) = registry.evaluate(&diffs, &state);
        assert_eq!(concurrent.len(), 1);

        for fut in concurrent {
            fut.await;
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // ── 3. ChangedTo does not fire when new value doesn't match ─────────

    #[tokio::test]
    async fn changed_to_does_not_fire_when_new_value_differs() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher(
            "status",
            WatchPredicate::ChangedTo(json!("active")),
            counter.clone(),
            false,
        ));

        let state = State::new();
        let diffs = vec![("status".to_string(), json!("inactive"), json!("pending"))];

        let (blocking, concurrent) = registry.evaluate(&diffs, &state);
        assert!(blocking.is_empty());
        assert!(concurrent.is_empty());
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    // ── 4. ChangedFrom fires only when old value matches ────────────────

    #[tokio::test]
    async fn changed_from_fires_when_old_value_matches() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher(
            "mode",
            WatchPredicate::ChangedFrom(json!("draft")),
            counter.clone(),
            false,
        ));

        let state = State::new();
        // Old is "draft" — should fire.
        let diffs = vec![("mode".to_string(), json!("draft"), json!("published"))];

        let (_, concurrent) = registry.evaluate(&diffs, &state);
        assert_eq!(concurrent.len(), 1);

        for fut in concurrent {
            fut.await;
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // Old is NOT "draft" — should not fire.
        let diffs2 = vec![("mode".to_string(), json!("published"), json!("archived"))];
        let (b, c) = registry.evaluate(&diffs2, &state);
        assert!(b.is_empty());
        assert!(c.is_empty());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // ── 5. CrossedAbove fires when crossing threshold upward ────────────

    #[tokio::test]
    async fn crossed_above_fires_on_upward_crossing() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher(
            "temp",
            WatchPredicate::CrossedAbove(100.0),
            counter.clone(),
            false,
        ));

        let state = State::new();
        // 95 -> 105: crosses above 100
        let diffs = vec![("temp".to_string(), json!(95.0), json!(105.0))];

        let (_, concurrent) = registry.evaluate(&diffs, &state);
        assert_eq!(concurrent.len(), 1);

        for fut in concurrent {
            fut.await;
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // ── 6. CrossedAbove does not fire when both above threshold ─────────

    #[tokio::test]
    async fn crossed_above_does_not_fire_when_both_above() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher(
            "temp",
            WatchPredicate::CrossedAbove(100.0),
            counter.clone(),
            false,
        ));

        let state = State::new();
        // 110 -> 120: both above 100, no crossing
        let diffs = vec![("temp".to_string(), json!(110.0), json!(120.0))];

        let (blocking, concurrent) = registry.evaluate(&diffs, &state);
        assert!(blocking.is_empty());
        assert!(concurrent.is_empty());
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    // ── 7. CrossedBelow fires when crossing threshold downward ──────────

    #[tokio::test]
    async fn crossed_below_fires_on_downward_crossing() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher(
            "battery",
            WatchPredicate::CrossedBelow(20.0),
            counter.clone(),
            false,
        ));

        let state = State::new();
        // 25 -> 15: crosses below 20
        let diffs = vec![("battery".to_string(), json!(25.0), json!(15.0))];

        let (_, concurrent) = registry.evaluate(&diffs, &state);
        assert_eq!(concurrent.len(), 1);

        for fut in concurrent {
            fut.await;
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // ── 8. BecameTrue fires when value changes to true ──────────────────

    #[tokio::test]
    async fn became_true_fires_on_false_to_true() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher(
            "flag",
            WatchPredicate::BecameTrue,
            counter.clone(),
            false,
        ));

        let state = State::new();
        let diffs = vec![("flag".to_string(), json!(false), json!(true))];

        let (_, concurrent) = registry.evaluate(&diffs, &state);
        assert_eq!(concurrent.len(), 1);

        for fut in concurrent {
            fut.await;
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // ── 9. BecameFalse fires when value changes from true to false ──────

    #[tokio::test]
    async fn became_false_fires_on_true_to_false() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher(
            "flag",
            WatchPredicate::BecameFalse,
            counter.clone(),
            false,
        ));

        let state = State::new();
        let diffs = vec![("flag".to_string(), json!(true), json!(false))];

        let (_, concurrent) = registry.evaluate(&diffs, &state);
        assert_eq!(concurrent.len(), 1);

        for fut in concurrent {
            fut.await;
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // ── 10. Custom predicate ────────────────────────────────────────────

    #[tokio::test]
    async fn custom_predicate_fires_when_fn_returns_true() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher(
            "score",
            WatchPredicate::Custom(Arc::new(|old, new| {
                // Fire only when value doubled
                match (as_f64(old), as_f64(new)) {
                    (Some(o), Some(n)) => (n - o * 2.0).abs() < f64::EPSILON,
                    _ => false,
                }
            })),
            counter.clone(),
            false,
        ));

        let state = State::new();
        // 5 -> 10: exactly doubled
        let diffs = vec![("score".to_string(), json!(5.0), json!(10.0))];

        let (_, concurrent) = registry.evaluate(&diffs, &state);
        assert_eq!(concurrent.len(), 1);

        for fut in concurrent {
            fut.await;
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // 5 -> 11: not doubled
        let diffs2 = vec![("score".to_string(), json!(5.0), json!(11.0))];
        let (b, c) = registry.evaluate(&diffs2, &state);
        assert!(b.is_empty());
        assert!(c.is_empty());
    }

    // ── 11. evaluate separates blocking vs concurrent futures ───────────

    #[tokio::test]
    async fn evaluate_separates_blocking_and_concurrent() {
        let blocking_counter = Arc::new(AtomicU32::new(0));
        let concurrent_counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();

        // Blocking watcher
        registry.add(counting_watcher(
            "x",
            WatchPredicate::Changed,
            blocking_counter.clone(),
            true,
        ));

        // Concurrent watcher
        registry.add(counting_watcher(
            "x",
            WatchPredicate::Changed,
            concurrent_counter.clone(),
            false,
        ));

        let state = State::new();
        let diffs = vec![("x".to_string(), json!(1), json!(2))];

        let (blocking, concurrent) = registry.evaluate(&diffs, &state);
        assert_eq!(blocking.len(), 1);
        assert_eq!(concurrent.len(), 1);

        // Execute both sets
        for fut in blocking {
            fut.await;
        }
        for fut in concurrent {
            fut.await;
        }

        assert_eq!(blocking_counter.load(Ordering::SeqCst), 1);
        assert_eq!(concurrent_counter.load(Ordering::SeqCst), 1);
    }

    // ── 12. evaluate with no matching diffs returns empty vecs ──────────

    #[test]
    fn evaluate_with_no_matching_diffs_returns_empty() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher("x", WatchPredicate::Changed, counter.clone(), false));

        let state = State::new();
        // Diff is for key "y", but watcher observes "x"
        let diffs = vec![("y".to_string(), json!(1), json!(2))];

        let (blocking, concurrent) = registry.evaluate(&diffs, &state);
        assert!(blocking.is_empty());
        assert!(concurrent.is_empty());
    }

    // ── 13. observed_keys tracks added watcher keys ─────────────────────

    #[test]
    fn observed_keys_tracks_added_watcher_keys() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();

        assert!(registry.observed_keys().is_empty());

        registry.add(counting_watcher("alpha", WatchPredicate::Changed, counter.clone(), false));
        registry.add(counting_watcher("beta", WatchPredicate::Changed, counter.clone(), false));
        registry.add(counting_watcher("alpha", WatchPredicate::BecameTrue, counter.clone(), true));

        let keys = registry.observed_keys();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains("alpha"));
        assert!(keys.contains("beta"));
    }

    // ── 14. multiple watchers on same key ───────────────────────────────

    #[tokio::test]
    async fn multiple_watchers_on_same_key() {
        let counter_a = Arc::new(AtomicU32::new(0));
        let counter_b = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();

        // Watcher A: fires on any change
        registry.add(counting_watcher("x", WatchPredicate::Changed, counter_a.clone(), false));

        // Watcher B: fires only when new == 42
        registry.add(counting_watcher(
            "x",
            WatchPredicate::ChangedTo(json!(42)),
            counter_b.clone(),
            false,
        ));

        let state = State::new();
        let diffs = vec![("x".to_string(), json!(1), json!(42))];

        let (_, concurrent) = registry.evaluate(&diffs, &state);
        // Both watchers should fire
        assert_eq!(concurrent.len(), 2);

        for fut in concurrent {
            fut.await;
        }
        assert_eq!(counter_a.load(Ordering::SeqCst), 1);
        assert_eq!(counter_b.load(Ordering::SeqCst), 1);

        // Now only watcher A should fire (new != 42)
        let diffs2 = vec![("x".to_string(), json!(42), json!(99))];
        let (_, concurrent2) = registry.evaluate(&diffs2, &state);
        assert_eq!(concurrent2.len(), 1);

        for fut in concurrent2 {
            fut.await;
        }
        assert_eq!(counter_a.load(Ordering::SeqCst), 2);
        assert_eq!(counter_b.load(Ordering::SeqCst), 1); // unchanged
    }

    // ── Additional edge-case tests ──────────────────────────────────────

    #[tokio::test]
    async fn action_receives_old_new_and_state() {
        let mut registry = WatcherRegistry::new();
        registry.add(recording_watcher("val", WatchPredicate::Changed, false));

        let state = State::new();
        let diffs = vec![("val".to_string(), json!("before"), json!("after"))];

        let (_, concurrent) = registry.evaluate(&diffs, &state);
        assert_eq!(concurrent.len(), 1);

        for fut in concurrent {
            fut.await;
        }

        assert_eq!(state.get_raw("recorded_old"), Some(json!("before")));
        assert_eq!(state.get_raw("recorded_new"), Some(json!("after")));
    }

    #[test]
    fn crossed_above_with_non_numeric_values_does_not_fire() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher(
            "x",
            WatchPredicate::CrossedAbove(10.0),
            counter.clone(),
            false,
        ));

        let state = State::new();
        let diffs = vec![("x".to_string(), json!("low"), json!("high"))];

        let (blocking, concurrent) = registry.evaluate(&diffs, &state);
        assert!(blocking.is_empty());
        assert!(concurrent.is_empty());
    }

    #[test]
    fn became_true_does_not_fire_on_non_bool() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher(
            "x",
            WatchPredicate::BecameTrue,
            counter.clone(),
            false,
        ));

        let state = State::new();
        // "truthy" string is not json bool true
        let diffs = vec![("x".to_string(), json!(0), json!("true"))];

        let (blocking, concurrent) = registry.evaluate(&diffs, &state);
        assert!(blocking.is_empty());
        assert!(concurrent.is_empty());
    }

    #[test]
    fn empty_diffs_produce_no_futures() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut registry = WatcherRegistry::new();
        registry.add(counting_watcher("x", WatchPredicate::Changed, counter.clone(), false));

        let state = State::new();
        let diffs: Vec<(String, Value, Value)> = vec![];

        let (blocking, concurrent) = registry.evaluate(&diffs, &state);
        assert!(blocking.is_empty());
        assert!(concurrent.is_empty());
    }

    #[test]
    fn default_creates_empty_registry() {
        let registry = WatcherRegistry::default();
        assert!(registry.observed_keys().is_empty());
    }
}
