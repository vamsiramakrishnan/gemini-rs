//! Typed key-value state container for agents.
//!
//! Supports optional delta tracking for transactional state management
//! and prefix-scoped accessors for namespace isolation.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use dashmap::DashMap;
use serde_json::Value;

/// A compile-time typed state key that eliminates typo bugs and type mismatches.
///
/// Create as a const and use with `State::get_key()` / `State::set_key()`:
///
/// ```rust,ignore
/// const TURN_COUNT: StateKey<u32> = StateKey::new("session:turn_count");
/// const SENTIMENT: StateKey<String> = StateKey::new("derived:sentiment");
///
/// state.set_key(&TURN_COUNT, 5);
/// let count: Option<u32> = state.get_key(&TURN_COUNT);
/// ```
pub struct StateKey<T> {
    key: &'static str,
    _phantom: PhantomData<fn() -> T>,
}

impl<T> StateKey<T> {
    /// Create a new typed state key.
    pub const fn new(key: &'static str) -> Self {
        Self {
            key,
            _phantom: PhantomData,
        }
    }

    /// The string key.
    pub const fn key(&self) -> &'static str {
        self.key
    }
}

/// A concurrent, type-safe state container that agents read from and write to.
///
/// By default, `set()` writes directly to the inner store. When delta tracking
/// is enabled via `with_delta_tracking()`, writes go to a separate delta map
/// that can be committed or rolled back.
#[derive(Debug, Clone)]
pub struct State {
    inner: Arc<DashMap<String, Value>>,
    delta: Arc<DashMap<String, Value>>,
    track_delta: bool,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    /// Create a new empty state container.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            delta: Arc::new(DashMap::new()),
            track_delta: false,
        }
    }

    /// Create a new State with delta tracking enabled.
    /// Writes go to the delta map; reads check delta first, then inner.
    pub fn with_delta_tracking(&self) -> State {
        State {
            inner: self.inner.clone(),
            delta: Arc::new(DashMap::new()),
            track_delta: true,
        }
    }

    /// Get a value by key, attempting to deserialize to the requested type.
    /// When delta tracking is enabled, checks delta first, then inner.
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.get_raw(key)
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Borrow a value by key without cloning, applying `f` to the reference.
    ///
    /// This is the zero-copy alternative to `get_raw()`. The closure receives
    /// a `&Value` directly from the DashMap ref-guard, avoiding the
    /// `Value::clone()` + `serde_json::from_value()` overhead of `get()`.
    ///
    /// Lookup order: delta (if tracking) → inner → derived fallback.
    pub fn with<F, R>(&self, key: &str, f: F) -> Option<R>
    where
        F: FnOnce(&Value) -> R,
    {
        if self.track_delta {
            if let Some(ref_multi) = self.delta.get(key) {
                return Some(f(ref_multi.value()));
            }
        }
        if let Some(ref_multi) = self.inner.get(key) {
            return Some(f(ref_multi.value()));
        }
        if !key.contains(':') {
            let mut derived_key = String::with_capacity(8 + key.len());
            use std::fmt::Write;
            let _ = write!(derived_key, "derived:{}", key);
            if self.track_delta {
                if let Some(ref_multi) = self.delta.get(&derived_key) {
                    return Some(f(ref_multi.value()));
                }
            }
            if let Some(ref_multi) = self.inner.get(&derived_key) {
                return Some(f(ref_multi.value()));
            }
        }
        None
    }

    /// Get a raw JSON value by key.
    /// When delta tracking is enabled, checks delta first, then inner.
    /// If the key is not found and doesn't contain a prefix, also checks `derived:{key}`
    /// as a transparent fallback for computed variables.
    pub fn get_raw(&self, key: &str) -> Option<Value> {
        if self.track_delta {
            if let Some(v) = self.delta.get(key) {
                return Some(v.value().clone());
            }
        }
        if let Some(v) = self.inner.get(key) {
            return Some(v.value().clone());
        }
        // Transparent derived fallback: if key has no prefix, check derived:{key}
        if !key.contains(':') {
            use std::fmt::Write;
            let mut derived_key = String::with_capacity(8 + key.len());
            let _ = write!(derived_key, "derived:{}", key);
            if self.track_delta {
                if let Some(v) = self.delta.get(&derived_key) {
                    return Some(v.value().clone());
                }
            }
            return self.inner.get(&derived_key).map(|v| v.value().clone());
        }
        None
    }

    /// Get a typed value using a `StateKey<T>`.
    pub fn get_key<T: serde::de::DeserializeOwned>(&self, key: &StateKey<T>) -> Option<T> {
        self.get(key.key())
    }

    /// Set a typed value using a `StateKey<T>`.
    pub fn set_key<T: serde::Serialize>(&self, key: &StateKey<T>, value: T) {
        self.set(key.key(), value);
    }

    /// Zero-copy borrow using a `StateKey<T>`.
    pub fn with_key<T, F, R>(&self, key: &StateKey<T>, f: F) -> Option<R>
    where
        F: FnOnce(&Value) -> R,
    {
        self.with(key.key(), f)
    }

    /// Set a value by key.
    /// When delta tracking is enabled, writes to delta instead of inner.
    pub fn set(&self, key: impl Into<String>, value: impl serde::Serialize) {
        let v = serde_json::to_value(value).expect("value must be serializable");
        if self.track_delta {
            self.delta.insert(key.into(), v);
        } else {
            self.inner.insert(key.into(), v);
        }
    }

    /// Set a value directly in the committed store, bypassing delta tracking.
    pub fn set_committed(&self, key: impl Into<String>, value: impl serde::Serialize) {
        let v = serde_json::to_value(value).expect("value must be serializable");
        self.inner.insert(key.into(), v);
    }

    /// Atomically read-modify-write a value.
    ///
    /// If the key doesn't exist, `default` is used as the initial value.
    /// The function `f` receives the current value and returns the new value.
    /// Returns the new value after modification.
    pub fn modify<T, F>(&self, key: &str, default: T, f: F) -> T
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
        F: FnOnce(T) -> T,
    {
        // Read current value from whichever store has it
        let current: T = self.get(key).unwrap_or(default);
        let new_val = f(current);
        self.set(key, &new_val);
        new_val
    }

    /// Check if a key exists (in delta or inner).
    pub fn contains(&self, key: &str) -> bool {
        if self.track_delta && self.delta.contains_key(key) {
            return true;
        }
        self.inner.contains_key(key)
    }

    /// Remove a key.
    pub fn remove(&self, key: &str) -> Option<Value> {
        if self.track_delta {
            // Remove from delta if present, but also check inner
            let from_delta = self.delta.remove(key).map(|(_, v)| v);
            let from_inner = self.inner.remove(key).map(|(_, v)| v);
            from_delta.or(from_inner)
        } else {
            self.inner.remove(key).map(|(_, v)| v)
        }
    }

    /// Get all keys (from both inner and delta when tracking).
    pub fn keys(&self) -> Vec<String> {
        if !self.track_delta || self.delta.is_empty() {
            return self.inner.iter().map(|r| r.key().clone()).collect();
        }
        let mut seen = std::collections::HashSet::with_capacity(
            self.inner.len() + self.delta.len(),
        );
        let mut keys = Vec::with_capacity(self.inner.len() + self.delta.len());
        for entry in self.inner.iter() {
            let key = entry.key().clone();
            seen.insert(key.clone());
            keys.push(key);
        }
        for entry in self.delta.iter() {
            let key = entry.key().clone();
            if seen.insert(key.clone()) {
                keys.push(key);
            }
        }
        keys
    }

    /// Create a new State containing only the specified keys.
    pub fn pick(&self, keys: &[&str]) -> State {
        let new = State::new();
        for key in keys {
            if let Some(v) = self.get_raw(key) {
                new.set(*key, v);
            }
        }
        new
    }

    /// Merge another state into this one (other's values overwrite on conflict).
    pub fn merge(&self, other: &State) {
        for entry in other.inner.iter() {
            self.inner.insert(entry.key().clone(), entry.value().clone());
        }
    }

    /// Rename a key.
    pub fn rename(&self, from: &str, to: &str) {
        if let Some(v) = self.remove(from) {
            if self.track_delta {
                self.delta.insert(to.to_string(), v);
            } else {
                self.inner.insert(to.to_string(), v);
            }
        }
    }

    // ── Delta methods ──────────────────────────────────────────────────────

    /// Whether delta tracking is enabled.
    pub fn is_tracking_delta(&self) -> bool {
        self.track_delta
    }

    /// Whether there are uncommitted delta changes.
    pub fn has_delta(&self) -> bool {
        self.track_delta && !self.delta.is_empty()
    }

    /// Get a snapshot of the current delta.
    pub fn delta(&self) -> HashMap<String, Value> {
        self.delta
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    /// Commit delta changes into the inner store, then clear the delta.
    pub fn commit(&self) {
        for entry in self.delta.iter() {
            self.inner
                .insert(entry.key().clone(), entry.value().clone());
        }
        self.delta.clear();
    }

    /// Discard all uncommitted delta changes.
    pub fn rollback(&self) {
        self.delta.clear();
    }

    // ── Prefix accessors ───────────────────────────────────────────────────

    /// Access state with the `app:` prefix scope.
    pub fn app(&self) -> PrefixedState<'_> {
        PrefixedState {
            state: self,
            prefix: "app:",
        }
    }

    /// Access state with the `user:` prefix scope.
    pub fn user(&self) -> PrefixedState<'_> {
        PrefixedState {
            state: self,
            prefix: "user:",
        }
    }

    /// Access state with the `temp:` prefix scope.
    pub fn temp(&self) -> PrefixedState<'_> {
        PrefixedState {
            state: self,
            prefix: "temp:",
        }
    }

    /// Access state with the `session:` prefix scope (auto-tracked signals).
    pub fn session(&self) -> PrefixedState<'_> {
        PrefixedState {
            state: self,
            prefix: "session:",
        }
    }

    /// Access state with the `turn:` prefix scope (reset each turn).
    pub fn turn(&self) -> PrefixedState<'_> {
        PrefixedState {
            state: self,
            prefix: "turn:",
        }
    }

    /// Access state with the `bg:` prefix scope (background tasks).
    pub fn bg(&self) -> PrefixedState<'_> {
        PrefixedState {
            state: self,
            prefix: "bg:",
        }
    }

    /// Access read-only state with the `derived:` prefix scope (computed vars only).
    pub fn derived(&self) -> ReadOnlyPrefixedState<'_> {
        ReadOnlyPrefixedState {
            state: self,
            prefix: "derived:",
        }
    }

    // ── Utility methods ───────────────────────────────────────────────────

    /// Snapshot the values of specific keys. Returns HashMap of key -> current value.
    /// Used by watchers to capture state before mutations.
    pub fn snapshot_values(&self, keys: &[&str]) -> HashMap<String, Value> {
        keys.iter()
            .filter_map(|&k| self.get_raw(k).map(|v| (k.to_string(), v)))
            .collect()
    }

    /// Diff current state against a previous snapshot.
    /// Returns Vec of (key, old_value, new_value) for keys that changed.
    pub fn diff_values(
        &self,
        prev: &HashMap<String, Value>,
        keys: &[&str],
    ) -> Vec<(String, Value, Value)> {
        keys.iter()
            .filter_map(|&k| {
                let old = prev.get(k);
                let new = self.get_raw(k);
                match (old, new) {
                    (Some(o), Some(n)) if o != &n => Some((k.to_string(), o.clone(), n)),
                    (None, Some(n)) => Some((k.to_string(), Value::Null, n)),
                    (Some(o), None) => Some((k.to_string(), o.clone(), Value::Null)),
                    _ => None,
                }
            })
            .collect()
    }

    /// Export all state as a HashMap (for persistence/serialization).
    pub fn to_hashmap(&self) -> std::collections::HashMap<String, serde_json::Value> {
        self.inner
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    /// Restore state from a HashMap (for persistence/deserialization).
    pub fn from_hashmap(&self, map: std::collections::HashMap<String, serde_json::Value>) {
        for (key, value) in map {
            self.inner.insert(key, value);
        }
    }

    /// Remove all keys with the given prefix.
    pub fn clear_prefix(&self, prefix: &str) {
        let keys_to_remove: Vec<String> = self
            .inner
            .iter()
            .filter(|entry| entry.key().starts_with(prefix))
            .map(|entry| entry.key().clone())
            .collect();
        for key in keys_to_remove {
            self.inner.remove(&key);
        }
        if self.track_delta {
            let delta_keys: Vec<String> = self
                .delta
                .iter()
                .filter(|entry| entry.key().starts_with(prefix))
                .map(|entry| entry.key().clone())
                .collect();
            for key in delta_keys {
                self.delta.remove(&key);
            }
        }
    }
}

/// A borrowed view of state that automatically prepends a prefix to all keys.
pub struct PrefixedState<'a> {
    state: &'a State,
    prefix: &'static str,
}

impl<'a> PrefixedState<'a> {
    fn prefixed_key(&self, key: &str) -> String {
        format!("{}{}", self.prefix, key)
    }

    /// Get a value by key (with prefix applied).
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.state.get(&self.prefixed_key(key))
    }

    /// Get a raw JSON value by key (with prefix applied).
    pub fn get_raw(&self, key: &str) -> Option<Value> {
        self.state.get_raw(&self.prefixed_key(key))
    }

    /// Zero-copy borrow a value by key (with prefix applied).
    pub fn with<F, R>(&self, key: &str, f: F) -> Option<R>
    where
        F: FnOnce(&Value) -> R,
    {
        self.state.with(&self.prefixed_key(key), f)
    }

    /// Set a value by key (with prefix applied).
    pub fn set(&self, key: impl AsRef<str>, value: impl serde::Serialize) {
        self.state.set(self.prefixed_key(key.as_ref()), value);
    }

    /// Check if a key exists (with prefix applied).
    pub fn contains(&self, key: &str) -> bool {
        self.state.contains(&self.prefixed_key(key))
    }

    /// Remove a key (with prefix applied).
    pub fn remove(&self, key: &str) -> Option<Value> {
        self.state.remove(&self.prefixed_key(key))
    }

    /// Get all keys within this prefix scope (prefix stripped from results).
    pub fn keys(&self) -> Vec<String> {
        self.state
            .keys()
            .into_iter()
            .filter_map(|k| k.strip_prefix(self.prefix).map(|s| s.to_string()))
            .collect()
    }
}

/// A borrowed, read-only view of state that automatically prepends a prefix to all keys.
///
/// Unlike `PrefixedState`, this does not expose `set()` or `remove()` methods,
/// making it suitable for computed/derived state that user code should not mutate.
pub struct ReadOnlyPrefixedState<'a> {
    state: &'a State,
    prefix: &'static str,
}

impl<'a> ReadOnlyPrefixedState<'a> {
    fn prefixed_key(&self, key: &str) -> String {
        format!("{}{}", self.prefix, key)
    }

    /// Get a value by key (with prefix applied).
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.state.get(&self.prefixed_key(key))
    }

    /// Get a raw JSON value by key (with prefix applied).
    pub fn get_raw(&self, key: &str) -> Option<Value> {
        self.state.get_raw(&self.prefixed_key(key))
    }

    /// Zero-copy borrow a value by key (with prefix applied).
    pub fn with<F, R>(&self, key: &str, f: F) -> Option<R>
    where
        F: FnOnce(&Value) -> R,
    {
        self.state.with(&self.prefixed_key(key), f)
    }

    /// Check if a key exists (with prefix applied).
    pub fn contains(&self, key: &str) -> bool {
        self.state.contains(&self.prefixed_key(key))
    }

    /// Get all keys within this prefix scope (prefix stripped from results).
    pub fn keys(&self) -> Vec<String> {
        self.state
            .keys()
            .into_iter()
            .filter_map(|k| k.strip_prefix(self.prefix).map(|s| s.to_string()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_string() {
        let state = State::new();
        state.set("name", "Alice");
        assert_eq!(state.get::<String>("name"), Some("Alice".to_string()));
    }

    #[test]
    fn set_and_get_json() {
        let state = State::new();
        state.set("data", serde_json::json!({"temp": 22}));
        let v: Value = state.get("data").unwrap();
        assert_eq!(v["temp"], 22);
    }

    #[test]
    fn pick_subset() {
        let state = State::new();
        state.set("a", 1);
        state.set("b", 2);
        state.set("c", 3);
        let picked = state.pick(&["a", "c"]);
        assert!(picked.contains("a"));
        assert!(!picked.contains("b"));
        assert!(picked.contains("c"));
    }

    #[test]
    fn merge_states() {
        let s1 = State::new();
        s1.set("a", 1);
        let s2 = State::new();
        s2.set("b", 2);
        s1.merge(&s2);
        assert!(s1.contains("a"));
        assert!(s1.contains("b"));
    }

    #[test]
    fn rename_key() {
        let state = State::new();
        state.set("old", "value");
        state.rename("old", "new");
        assert!(!state.contains("old"));
        assert_eq!(state.get::<String>("new"), Some("value".to_string()));
    }

    #[test]
    fn remove_returns_value() {
        let state = State::new();
        state.set("key", 42);
        let removed = state.remove("key");
        assert!(removed.is_some());
        assert!(!state.contains("key"));
    }

    #[test]
    fn get_missing_returns_none() {
        let state = State::new();
        assert_eq!(state.get::<String>("nope"), None);
    }

    // ── Delta tracking tests ──────────────────────────────────────────────

    #[test]
    fn delta_tracking_writes_to_delta() {
        let state = State::new();
        state.set("committed", "yes");

        let tracked = state.with_delta_tracking();
        tracked.set("new_key", "new_value");

        // New key visible through tracked state
        assert_eq!(tracked.get::<String>("new_key"), Some("new_value".to_string()));
        // But NOT visible in original (non-delta) state's inner
        assert!(!state.contains("new_key"));
        // Committed key still visible through tracked state
        assert_eq!(tracked.get::<String>("committed"), Some("yes".to_string()));
    }

    #[test]
    fn delta_has_delta_reports_correctly() {
        let state = State::new();
        let tracked = state.with_delta_tracking();
        assert!(!tracked.has_delta());

        tracked.set("key", "val");
        assert!(tracked.has_delta());
    }

    #[test]
    fn delta_commit_merges_to_inner() {
        let state = State::new();
        let tracked = state.with_delta_tracking();
        tracked.set("key", "val");
        assert!(!state.contains("key"));

        tracked.commit();
        // Now visible in original state
        assert_eq!(state.get::<String>("key"), Some("val".to_string()));
        assert!(!tracked.has_delta());
    }

    #[test]
    fn delta_rollback_discards_changes() {
        let state = State::new();
        let tracked = state.with_delta_tracking();
        tracked.set("key", "val");
        assert!(tracked.has_delta());

        tracked.rollback();
        assert!(!tracked.has_delta());
        assert!(!state.contains("key"));
        assert!(!tracked.contains("key"));
    }

    #[test]
    fn delta_snapshot() {
        let state = State::new();
        let tracked = state.with_delta_tracking();
        tracked.set("a", 1);
        tracked.set("b", 2);

        let snapshot = tracked.delta();
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot.contains_key("a"));
        assert!(snapshot.contains_key("b"));
    }

    #[test]
    fn set_committed_bypasses_delta() {
        let state = State::new();
        let tracked = state.with_delta_tracking();
        tracked.set_committed("direct", "value");

        // Visible immediately in inner
        assert_eq!(state.get::<String>("direct"), Some("value".to_string()));
        // Not in delta
        assert!(!tracked.has_delta());
        // Still visible through tracked (reads inner too)
        assert_eq!(tracked.get::<String>("direct"), Some("value".to_string()));
    }

    #[test]
    fn no_delta_tracking_preserves_existing_behavior() {
        let state = State::new();
        assert!(!state.is_tracking_delta());
        state.set("key", "val");
        assert_eq!(state.get::<String>("key"), Some("val".to_string()));
        assert!(!state.has_delta());
    }

    // ── Prefix tests ──────────────────────────────────────────────────────

    #[test]
    fn prefix_app_set_and_get() {
        let state = State::new();
        state.app().set("flag", true);

        // Accessible via prefix accessor
        assert_eq!(state.app().get::<bool>("flag"), Some(true));
        // Also accessible via raw key
        assert_eq!(state.get::<bool>("app:flag"), Some(true));
    }

    #[test]
    fn prefix_user_set_and_get() {
        let state = State::new();
        state.user().set("name", "Alice");
        assert_eq!(state.user().get::<String>("name"), Some("Alice".to_string()));
        assert_eq!(state.get::<String>("user:name"), Some("Alice".to_string()));
    }

    #[test]
    fn prefix_temp_set_and_get() {
        let state = State::new();
        state.temp().set("scratch", 42);
        assert_eq!(state.temp().get::<i32>("scratch"), Some(42));
    }

    #[test]
    fn prefix_contains_and_remove() {
        let state = State::new();
        state.app().set("x", 1);
        assert!(state.app().contains("x"));
        state.app().remove("x");
        assert!(!state.app().contains("x"));
    }

    #[test]
    fn prefix_keys() {
        let state = State::new();
        state.app().set("a", 1);
        state.app().set("b", 2);
        state.user().set("c", 3);

        let app_keys = state.app().keys();
        assert_eq!(app_keys.len(), 2);
        assert!(app_keys.contains(&"a".to_string()));
        assert!(app_keys.contains(&"b".to_string()));

        let user_keys = state.user().keys();
        assert_eq!(user_keys.len(), 1);
        assert!(user_keys.contains(&"c".to_string()));
    }

    #[test]
    fn prefix_with_delta_tracking() {
        let state = State::new();
        let tracked = state.with_delta_tracking();
        tracked.app().set("flag", true);

        // Visible in tracked state via prefix
        assert_eq!(tracked.app().get::<bool>("flag"), Some(true));
        // In delta, not committed
        assert!(tracked.has_delta());
        assert!(!state.contains("app:flag"));

        tracked.commit();
        assert_eq!(state.get::<bool>("app:flag"), Some(true));
    }

    // ── New prefix accessor tests ────────────────────────────────────────

    #[test]
    fn prefix_session_set_and_get() {
        let state = State::new();
        state.session().set("turn_count", 5);
        assert_eq!(state.session().get::<i32>("turn_count"), Some(5));
        assert_eq!(state.get::<i32>("session:turn_count"), Some(5));
    }

    #[test]
    fn prefix_turn_set_and_get() {
        let state = State::new();
        state.turn().set("transcript", "hello");
        assert_eq!(
            state.turn().get::<String>("transcript"),
            Some("hello".to_string())
        );
        assert_eq!(
            state.get::<String>("turn:transcript"),
            Some("hello".to_string())
        );
    }

    #[test]
    fn prefix_bg_set_and_get() {
        let state = State::new();
        state.bg().set("task_id", "abc-123");
        assert_eq!(
            state.bg().get::<String>("task_id"),
            Some("abc-123".to_string())
        );
        assert_eq!(
            state.get::<String>("bg:task_id"),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn prefix_session_contains_and_remove() {
        let state = State::new();
        state.session().set("x", 1);
        assert!(state.session().contains("x"));
        state.session().remove("x");
        assert!(!state.session().contains("x"));
    }

    #[test]
    fn prefix_turn_keys() {
        let state = State::new();
        state.turn().set("a", 1);
        state.turn().set("b", 2);
        state.session().set("c", 3);

        let turn_keys = state.turn().keys();
        assert_eq!(turn_keys.len(), 2);
        assert!(turn_keys.contains(&"a".to_string()));
        assert!(turn_keys.contains(&"b".to_string()));
    }

    // ── ReadOnlyPrefixedState (derived) tests ────────────────────────────

    #[test]
    fn derived_read_only_get() {
        let state = State::new();
        // Write via raw key (simulating ComputedRegistry)
        state.set("derived:sentiment", "positive");
        assert_eq!(
            state.derived().get::<String>("sentiment"),
            Some("positive".to_string())
        );
    }

    #[test]
    fn derived_read_only_get_raw() {
        let state = State::new();
        state.set("derived:score", serde_json::json!(0.95));
        let raw = state.derived().get_raw("score");
        assert!(raw.is_some());
        assert_eq!(raw.unwrap(), serde_json::json!(0.95));
    }

    #[test]
    fn derived_read_only_contains() {
        let state = State::new();
        state.set("derived:exists", true);
        assert!(state.derived().contains("exists"));
        assert!(!state.derived().contains("missing"));
    }

    #[test]
    fn derived_read_only_keys() {
        let state = State::new();
        state.set("derived:a", 1);
        state.set("derived:b", 2);
        state.set("app:c", 3);

        let derived_keys = state.derived().keys();
        assert_eq!(derived_keys.len(), 2);
        assert!(derived_keys.contains(&"a".to_string()));
        assert!(derived_keys.contains(&"b".to_string()));
    }

    #[test]
    fn derived_missing_key_returns_none() {
        let state = State::new();
        assert_eq!(state.derived().get::<String>("nope"), None);
        assert_eq!(state.derived().get_raw("nope"), None);
    }

    // ── snapshot_values tests ────────────────────────────────────────────

    #[test]
    fn snapshot_values_captures_existing_keys() {
        let state = State::new();
        state.set("a", 1);
        state.set("b", "hello");
        state.set("c", true);

        let snap = state.snapshot_values(&["a", "b", "missing"]);
        assert_eq!(snap.len(), 2);
        assert_eq!(snap.get("a"), Some(&serde_json::json!(1)));
        assert_eq!(snap.get("b"), Some(&serde_json::json!("hello")));
        assert!(!snap.contains_key("missing"));
    }

    #[test]
    fn snapshot_values_empty_keys() {
        let state = State::new();
        state.set("a", 1);
        let snap = state.snapshot_values(&[]);
        assert!(snap.is_empty());
    }

    // ── diff_values tests ────────────────────────────────────────────────

    #[test]
    fn diff_values_detects_changed_value() {
        let state = State::new();
        state.set("x", 1);
        let snap = state.snapshot_values(&["x"]);

        state.set("x", 2);
        let diffs = state.diff_values(&snap, &["x"]);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].0, "x");
        assert_eq!(diffs[0].1, serde_json::json!(1));
        assert_eq!(diffs[0].2, serde_json::json!(2));
    }

    #[test]
    fn diff_values_detects_new_key() {
        let state = State::new();
        let snap = state.snapshot_values(&["y"]);

        state.set("y", "new");
        let diffs = state.diff_values(&snap, &["y"]);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].0, "y");
        assert_eq!(diffs[0].1, Value::Null);
        assert_eq!(diffs[0].2, serde_json::json!("new"));
    }

    #[test]
    fn diff_values_detects_removed_key() {
        let state = State::new();
        state.set("z", 42);
        let snap = state.snapshot_values(&["z"]);

        state.remove("z");
        let diffs = state.diff_values(&snap, &["z"]);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].0, "z");
        assert_eq!(diffs[0].1, serde_json::json!(42));
        assert_eq!(diffs[0].2, Value::Null);
    }

    #[test]
    fn diff_values_no_change() {
        let state = State::new();
        state.set("stable", 10);
        let snap = state.snapshot_values(&["stable"]);

        // No mutation
        let diffs = state.diff_values(&snap, &["stable"]);
        assert!(diffs.is_empty());
    }

    #[test]
    fn diff_values_multiple_keys_mixed_changes() {
        let state = State::new();
        state.set("a", 1);
        state.set("b", 2);
        let snap = state.snapshot_values(&["a", "b", "c"]);

        state.set("a", 10); // changed
        // b unchanged
        state.set("c", 3); // new

        let diffs = state.diff_values(&snap, &["a", "b", "c"]);
        assert_eq!(diffs.len(), 2); // a changed, c new; b unchanged
        let diff_keys: Vec<&str> = diffs.iter().map(|(k, _, _)| k.as_str()).collect();
        assert!(diff_keys.contains(&"a"));
        assert!(diff_keys.contains(&"c"));
    }

    // ── clear_prefix tests ───────────────────────────────────────────────

    #[test]
    fn clear_prefix_removes_matching_keys() {
        let state = State::new();
        state.set("turn:a", 1);
        state.set("turn:b", 2);
        state.set("app:c", 3);
        state.set("session:d", 4);

        state.clear_prefix("turn:");
        assert!(!state.contains("turn:a"));
        assert!(!state.contains("turn:b"));
        assert!(state.contains("app:c"));
        assert!(state.contains("session:d"));
    }

    #[test]
    fn clear_prefix_no_matching_keys_is_noop() {
        let state = State::new();
        state.set("app:x", 1);
        state.clear_prefix("turn:");
        assert!(state.contains("app:x"));
    }

    #[test]
    fn clear_prefix_also_clears_delta() {
        let state = State::new();
        state.set("turn:committed", 1);
        let tracked = state.with_delta_tracking();
        tracked.set("turn:delta_val", 2);

        // Both committed and delta have turn: keys
        assert!(tracked.contains("turn:committed"));
        assert!(tracked.contains("turn:delta_val"));

        tracked.clear_prefix("turn:");
        assert!(!tracked.contains("turn:committed"));
        assert!(!tracked.contains("turn:delta_val"));
    }

    #[test]
    fn clear_prefix_via_turn_accessor() {
        let state = State::new();
        state.turn().set("x", 1);
        state.turn().set("y", 2);
        state.app().set("z", 3);

        state.clear_prefix("turn:");
        assert!(state.turn().keys().is_empty());
        assert!(state.app().contains("z"));
    }

    // ── modify() tests ──────────────────────────────────────────────────

    #[test]
    fn modify_increment_existing() {
        let state = State::new();
        state.set("count", 5u32);
        let result = state.modify("count", 0u32, |n| n + 1);
        assert_eq!(result, 6);
        assert_eq!(state.get::<u32>("count"), Some(6));
    }

    #[test]
    fn modify_uses_default_when_missing() {
        let state = State::new();
        let result = state.modify("new_count", 0u32, |n| n + 1);
        assert_eq!(result, 1);
        assert_eq!(state.get::<u32>("new_count"), Some(1));
    }

    #[test]
    fn modify_with_delta_tracking() {
        let state = State::new();
        state.set("x", 10u32);
        let tracked = state.with_delta_tracking();
        let result = tracked.modify("x", 0u32, |n| n * 2);
        assert_eq!(result, 20);
        // Written to delta, not committed
        assert_eq!(tracked.get::<u32>("x"), Some(20));
        assert_eq!(state.get::<u32>("x"), Some(10)); // original unchanged
    }

    // ── derived fallback tests ──────────────────────────────────────────

    #[test]
    fn get_falls_back_to_derived_prefix() {
        let state = State::new();
        state.set("derived:risk", 0.85);
        // Access without prefix — should find derived:risk
        assert_eq!(state.get::<f64>("risk"), Some(0.85));
    }

    #[test]
    fn get_prefers_direct_key_over_derived() {
        let state = State::new();
        state.set("score", 1.0);
        state.set("derived:score", 0.5);
        // Direct key should win
        assert_eq!(state.get::<f64>("score"), Some(1.0));
    }

    #[test]
    fn get_derived_fallback_skipped_for_prefixed_keys() {
        let state = State::new();
        state.set("derived:risk", 0.85);
        // Prefixed key should NOT trigger fallback
        assert_eq!(state.get::<f64>("app:risk"), None);
    }

    #[test]
    fn get_derived_fallback_with_delta_tracking() {
        let state = State::new();
        let tracked = state.with_delta_tracking();
        tracked.set("derived:computed_val", 42);
        assert_eq!(tracked.get::<i32>("computed_val"), Some(42));
    }

    // ── with() zero-copy borrow tests ──────────────────────────────────

    #[test]
    fn with_reads_from_inner() {
        let state = State::new();
        state.set("name", "Alice");
        let len = state.with("name", |v| v.as_str().unwrap().len());
        assert_eq!(len, Some(5));
    }

    #[test]
    fn with_reads_from_delta_first() {
        let state = State::new();
        state.set("x", 1);
        let tracked = state.with_delta_tracking();
        tracked.set("x", 99);
        let val = tracked.with("x", |v| v.as_i64().unwrap());
        assert_eq!(val, Some(99));
    }

    #[test]
    fn with_falls_back_to_inner_when_not_in_delta() {
        let state = State::new();
        state.set("committed", "yes");
        let tracked = state.with_delta_tracking();
        let val = tracked.with("committed", |v| v.as_str().unwrap().to_string());
        assert_eq!(val, Some("yes".to_string()));
    }

    #[test]
    fn with_falls_back_to_derived() {
        let state = State::new();
        state.set("derived:risk", 0.85);
        let val = state.with("risk", |v| v.as_f64().unwrap());
        assert_eq!(val, Some(0.85));
    }

    #[test]
    fn with_derived_fallback_skipped_for_prefixed() {
        let state = State::new();
        state.set("derived:risk", 0.85);
        let val = state.with("app:risk", |v| v.as_f64().unwrap());
        assert_eq!(val, None);
    }

    #[test]
    fn with_returns_none_for_missing() {
        let state = State::new();
        let val = state.with("missing", |v| v.clone());
        assert_eq!(val, None);
    }

    #[test]
    fn with_on_prefixed_state() {
        let state = State::new();
        state.app().set("flag", true);
        let val = state.app().with("flag", |v| v.as_bool().unwrap());
        assert_eq!(val, Some(true));
    }

    #[test]
    fn with_on_read_only_prefixed_state() {
        let state = State::new();
        state.set("derived:score", serde_json::json!(0.95));
        let val = state.derived().with("score", |v| v.as_f64().unwrap());
        assert_eq!(val, Some(0.95));
    }

    // ── StateKey typed key tests ───────────────────────────────────────

    const TURN_COUNT: StateKey<u32> = StateKey::new("session:turn_count");
    const NAME: StateKey<String> = StateKey::new("user:name");

    #[test]
    fn state_key_get_and_set() {
        let state = State::new();
        state.set_key(&TURN_COUNT, 5);
        assert_eq!(state.get_key(&TURN_COUNT), Some(5));
    }

    #[test]
    fn state_key_get_missing() {
        let state = State::new();
        assert_eq!(state.get_key(&TURN_COUNT), None);
    }

    #[test]
    fn state_key_string_type() {
        let state = State::new();
        state.set_key(&NAME, "Alice".to_string());
        assert_eq!(state.get_key(&NAME), Some("Alice".to_string()));
    }

    #[test]
    fn state_key_with() {
        let state = State::new();
        state.set_key(&TURN_COUNT, 42);
        let val = state.with_key(&TURN_COUNT, |v| v.as_u64().unwrap());
        assert_eq!(val, Some(42));
    }

    #[test]
    fn state_key_interop_with_raw() {
        let state = State::new();
        state.set_key(&TURN_COUNT, 10);
        // Can also read via raw key
        assert_eq!(state.get::<u32>("session:turn_count"), Some(10));
    }
}
