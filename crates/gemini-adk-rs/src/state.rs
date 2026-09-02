//! Typed key-value state container for agents.
//!
//! Supports optional delta tracking for transactional state management
//! and prefix-scoped accessors for namespace isolation.

use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use dashmap::DashMap;
use serde_json::Value;

const DEFAULT_MUTATION_JOURNAL_CAPACITY: usize = 1024;

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

/// Where a state mutation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateMutationOrigin {
    /// Regular `State::set` or prefixed state write.
    Set,
    /// Direct committed-store write that bypasses delta tracking.
    SetCommitted,
    /// Removal of a single key.
    Remove,
    /// Removal caused by clearing a prefix.
    ClearPrefix,
    /// Delta changes committed into the base state.
    Commit,
}

/// A single state mutation recorded in the bounded mutation journal.
///
/// Serializes to/from JSON for durable journaling (see [`JournalSink`]);
/// `timestamp` is encoded as integer milliseconds since the Unix epoch.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StateMutation {
    /// Monotonic sequence number assigned when the mutation was recorded.
    pub sequence: u64,
    /// State key that changed.
    pub key: String,
    /// Value before the mutation, or `None` when the key did not exist.
    pub old: Option<Value>,
    /// Value after the mutation, or `None` when the key was removed.
    pub new: Option<Value>,
    /// Operation that recorded the mutation.
    pub origin: StateMutationOrigin,
    /// Wall-clock time at which the mutation was recorded.
    /// Serialized as milliseconds since the Unix epoch (`timestamp_ms`).
    #[serde(rename = "timestamp_ms", with = "systemtime_epoch_millis")]
    pub timestamp: SystemTime,
    /// Whether the mutation was written to a delta-tracked view.
    pub delta: bool,
}

/// Serde codec mapping [`SystemTime`] to/from integer epoch milliseconds.
mod systemtime_epoch_millis {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(t: &SystemTime, ser: S) -> Result<S::Ok, S::Error> {
        let millis = t
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        ser.serialize_u64(millis)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<SystemTime, D::Error> {
        let millis = u64::deserialize(de)?;
        Ok(UNIX_EPOCH + Duration::from_millis(millis))
    }
}

/// Synchronous, durable sink for state mutations.
///
/// The in-memory mutation journal is a bounded ring (1024 entries) — long
/// sessions lose history. A `JournalSink` receives every mutation as it is
/// recorded so it can be persisted in full.
///
/// `write` runs on the state-write hot path (under the journal lock): it must
/// be cheap, must not await, and must not panic — implementations log internal
/// errors instead of surfacing them.
pub trait JournalSink: Send + Sync {
    /// Persist one mutation. Must not panic; log errors internally.
    fn write(&self, m: &StateMutation);
}

/// Shared, swappable [`JournalSink`] slot — one slot per [`State`] family
/// (clones and delta views share it, like the in-memory ring).
#[derive(Clone, Default)]
struct JournalSinkSlot(Arc<parking_lot::RwLock<Option<Arc<dyn JournalSink>>>>);

impl std::fmt::Debug for JournalSinkSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let installed = self.0.read().is_some();
        f.debug_tuple("JournalSinkSlot").field(&installed).finish()
    }
}

const JOURNAL_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Log a journal-sink internal error without panicking the write path
/// (journaling is infallible by contract, so the error is only reported).
fn journal_log_error(context: &'static str, e: &dyn std::fmt::Display) {
    tracing::warn!(error = %e, "{context}");
}

struct FileJournalInner {
    writer: std::io::BufWriter<std::fs::File>,
    last_flush: std::time::Instant,
}

/// Durable [`JournalSink`] writing one JSON object per line (JSONL).
///
/// Writes are buffered behind a `parking_lot::Mutex` and flushed at least
/// every second and on drop. I/O errors are logged via `tracing::warn!` —
/// journaling never panics a state write.
///
/// ```jsonl
/// {"sequence":1,"key":"app:last_city","old":null,"new":"London","origin":"set","timestamp_ms":1718000000000,"delta":false}
/// ```
pub struct FileJournalSink {
    inner: parking_lot::Mutex<FileJournalInner>,
}

impl FileJournalSink {
    /// Create (truncating) the journal file at `path`.
    pub fn create(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let file = std::fs::File::create(path)?;
        Ok(Self {
            inner: parking_lot::Mutex::new(FileJournalInner {
                writer: std::io::BufWriter::new(file),
                last_flush: std::time::Instant::now(),
            }),
        })
    }

    /// Flush buffered mutations to disk now.
    pub fn flush(&self) {
        let mut inner = self.inner.lock();
        if let Err(e) = std::io::Write::flush(&mut inner.writer) {
            journal_log_error("FileJournalSink flush failed", &e);
        }
        inner.last_flush = std::time::Instant::now();
    }
}

impl JournalSink for FileJournalSink {
    fn write(&self, m: &StateMutation) {
        let line = match serde_json::to_string(m) {
            Ok(line) => line,
            Err(e) => {
                journal_log_error("FileJournalSink serialize failed", &e);
                return;
            }
        };
        let mut inner = self.inner.lock();
        if let Err(e) = std::io::Write::write_all(&mut inner.writer, line.as_bytes())
            .and_then(|()| std::io::Write::write_all(&mut inner.writer, b"\n"))
        {
            journal_log_error("FileJournalSink write failed", &e);
            return;
        }
        if inner.last_flush.elapsed() >= JOURNAL_FLUSH_INTERVAL {
            if let Err(e) = std::io::Write::flush(&mut inner.writer) {
                journal_log_error("FileJournalSink flush failed", &e);
            }
            inner.last_flush = std::time::Instant::now();
        }
    }
}

impl Drop for FileJournalSink {
    fn drop(&mut self) {
        if let Err(e) = std::io::Write::flush(&mut self.inner.lock().writer) {
            journal_log_error("FileJournalSink final flush failed", &e);
        }
    }
}

/// In-memory [`JournalSink`] for tests and replay harnesses. Unbounded.
#[derive(Default)]
pub struct MemoryJournalSink {
    entries: parking_lot::Mutex<Vec<StateMutation>>,
}

impl MemoryJournalSink {
    /// Create an empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot all recorded mutations (in write order).
    pub fn entries(&self) -> Vec<StateMutation> {
        self.entries.lock().clone()
    }

    /// Number of recorded mutations.
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// Whether nothing has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }
}

impl JournalSink for MemoryJournalSink {
    fn write(&self, m: &StateMutation) {
        self.entries.lock().push(m.clone());
    }
}

/// Error returned by fallible state reads and writes.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// The value could not be serialized to JSON.
    #[error("failed to serialize state value for key '{key}': {source}")]
    Serialize {
        /// The key that was being written.
        key: String,
        /// The underlying serde error.
        source: serde_json::Error,
    },
    /// A value is present at the key but does not deserialize to the
    /// requested type (see [`State::try_get`]).
    #[error("state value at key '{key}' is not the requested type: {source}")]
    WrongType {
        /// The key that was being read.
        key: String,
        /// The underlying serde error.
        source: serde_json::Error,
    },
}

/// A pending write in a delta-tracked view.
///
/// Unlike a bare value, this distinguishes a *write* from a *removal* so that a
/// delta can record tombstones and `rollback()` can restore the base state
/// after removals and prefix clears.
#[derive(Debug, Clone)]
enum DeltaOp {
    /// Set the key to this value on commit.
    Put(Value),
    /// Remove the key on commit (tombstone — shadows the committed value).
    Delete,
}

/// Provenance and confidence for a single state slot — the evidence behind a
/// value, aggregated from the mutation journal and the `state_meta:{key}` record.
///
/// This is what lets the model confirm principled-ly ("I heard 6, right?"):
/// whether a slot was directly set, resolved from a system, or carries low
/// confidence, and when it last changed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SlotEvidence {
    /// The state key.
    pub key: String,
    /// Whether the key currently has a value.
    pub present: bool,
    /// The current value, if any.
    pub value: Option<Value>,
    /// Provenance source from `state_meta:{key}.source` (e.g. `agent`/`fetch`/
    /// `llm`/`extraction`), if recorded.
    pub source: Option<String>,
    /// Confidence from `state_meta:{key}.confidence` (0.0–1.0), if recorded.
    pub confidence: Option<f64>,
    /// Journal sequence of the most recent write to this key, if still in the
    /// bounded journal window.
    pub last_sequence: Option<u64>,
    /// Origin of the most recent recorded write, if known.
    pub last_origin: Option<StateMutationOrigin>,
}

/// A concurrent, type-safe state container that agents read from and write to.
///
/// By default, `set()` writes directly to the inner store. When delta tracking
/// is enabled via `with_delta_tracking()`, writes go to a separate delta map
/// (with tombstones) that can be atomically committed or rolled back.
#[derive(Debug, Clone)]
pub struct State {
    inner: Arc<DashMap<String, Value>>,
    delta: Arc<DashMap<String, DeltaOp>>,
    mutations: Arc<std::sync::Mutex<VecDeque<StateMutation>>>,
    next_mutation_sequence: Arc<AtomicU64>,
    mutation_capacity: usize,
    journal_sink: JournalSinkSlot,
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
            mutations: Arc::new(std::sync::Mutex::new(VecDeque::new())),
            next_mutation_sequence: Arc::new(AtomicU64::new(1)),
            mutation_capacity: DEFAULT_MUTATION_JOURNAL_CAPACITY,
            journal_sink: JournalSinkSlot::default(),
            track_delta: false,
        }
    }

    /// Create a new State with delta tracking enabled.
    /// Writes go to the delta map; reads check delta first, then inner.
    pub fn with_delta_tracking(&self) -> State {
        State {
            inner: self.inner.clone(),
            delta: Arc::new(DashMap::new()),
            mutations: self.mutations.clone(),
            next_mutation_sequence: self.next_mutation_sequence.clone(),
            mutation_capacity: self.mutation_capacity,
            journal_sink: self.journal_sink.clone(),
            track_delta: true,
        }
    }

    /// Install a durable [`JournalSink`] that receives every state mutation.
    ///
    /// The sink is shared with all clones and delta views of this `State`
    /// (like the in-memory ring) and is invoked synchronously on the write
    /// path — keep it cheap. The in-memory ring keeps serving
    /// [`recent_mutations`](Self::recent_mutations)/[`evidence`](Self::evidence);
    /// the sink adds unbounded durability.
    pub fn set_journal_sink(&self, sink: Arc<dyn JournalSink>) {
        *self.journal_sink.0.write() = Some(sink);
    }

    /// Builder-style variant of [`set_journal_sink`](Self::set_journal_sink).
    pub fn with_journal_sink(self, sink: Arc<dyn JournalSink>) -> Self {
        self.set_journal_sink(sink);
        self
    }

    /// Get a value by key, attempting to deserialize to the requested type.
    /// When delta tracking is enabled, checks delta first, then inner.
    ///
    /// This is the *lenient* read: a value that is present but of the wrong
    /// type is reported as `None`, indistinguishable from an absent key. Use
    /// [`try_get`](Self::try_get) when that distinction matters.
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.get_raw(key)
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Get a value by key, distinguishing "absent" from "present but the wrong
    /// type".
    ///
    /// Returns `Ok(None)` when no value is stored at `key` (after the same
    /// delta → inner → `derived:` lookup as [`get`](Self::get)), `Ok(Some(v))`
    /// when the stored value deserializes to `T`, and
    /// [`StateError::WrongType`] when a value exists but does not. This is the
    /// *strict* read; [`get`](Self::get) is the lenient form that folds the
    /// error case into `None`.
    pub fn try_get<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, StateError> {
        match self.get_raw(key) {
            None => Ok(None),
            Some(v) => {
                serde_json::from_value(v)
                    .map(Some)
                    .map_err(|source| StateError::WrongType {
                        key: key.to_string(),
                        source,
                    })
            }
        }
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
            match self.delta.get(key).map(|r| r.value().clone()) {
                Some(DeltaOp::Put(v)) => return Some(f(&v)),
                Some(DeltaOp::Delete) => return None, // tombstone shadows inner
                None => {}
            }
        }
        if let Some(ref_multi) = self.inner.get(key) {
            return Some(f(ref_multi.value()));
        }
        if !key.contains(':') {
            let mut derived_key = String::with_capacity(8 + key.len());
            use std::fmt::Write;
            let _ = write!(derived_key, "derived:{key}");
            if self.track_delta {
                match self.delta.get(&derived_key).map(|r| r.value().clone()) {
                    Some(DeltaOp::Put(v)) => return Some(f(&v)),
                    Some(DeltaOp::Delete) => return None,
                    None => {}
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
            match self.delta.get(key).map(|r| r.value().clone()) {
                Some(DeltaOp::Put(v)) => return Some(v),
                Some(DeltaOp::Delete) => return None, // tombstone shadows inner
                None => {}
            }
        }
        if let Some(v) = self.inner.get(key) {
            return Some(v.value().clone());
        }
        // Transparent derived fallback: if key has no prefix, check derived:{key}
        if !key.contains(':') {
            use std::fmt::Write;
            let mut derived_key = String::with_capacity(8 + key.len());
            let _ = write!(derived_key, "derived:{key}");
            if self.track_delta {
                match self.delta.get(&derived_key).map(|r| r.value().clone()) {
                    Some(DeltaOp::Put(v)) => return Some(v),
                    Some(DeltaOp::Delete) => return None,
                    None => {}
                }
            }
            return self.inner.get(&derived_key).map(|v| v.value().clone());
        }
        None
    }

    /// Get a typed value using a `StateKey<T>` (lenient — a wrong-typed value
    /// reads as `None`; see [`get`](Self::get)).
    pub fn get_key<T: serde::de::DeserializeOwned>(&self, key: &StateKey<T>) -> Option<T> {
        self.get(key.key())
    }

    /// Get a typed value using a `StateKey<T>`, distinguishing "absent" from
    /// "present but the wrong type" (see [`try_get`](Self::try_get)).
    pub fn try_get_key<T: serde::de::DeserializeOwned>(
        &self,
        key: &StateKey<T>,
    ) -> Result<Option<T>, StateError> {
        self.try_get(key.key())
    }

    /// Set a typed value using a `StateKey<T>`.
    ///
    /// Returns [`StateError`] if `value` cannot be serialized to JSON.
    pub fn set_key<T: serde::Serialize>(
        &self,
        key: &StateKey<T>,
        value: T,
    ) -> Result<(), StateError> {
        self.set(key.key(), value)
    }

    /// Zero-copy borrow using a `StateKey<T>`.
    pub fn with_key<T, F, R>(&self, key: &StateKey<T>, f: F) -> Option<R>
    where
        F: FnOnce(&Value) -> R,
    {
        self.with(key.key(), f)
    }

    /// Set a value by key.
    ///
    /// When delta tracking is enabled, writes to the delta view instead of the
    /// committed store. Returns [`StateError`] if `value` cannot be serialized
    /// to JSON — a public SDK write never panics on caller data.
    pub fn set(
        &self,
        key: impl Into<String>,
        value: impl serde::Serialize,
    ) -> Result<(), StateError> {
        let key = key.into();
        let v = serde_json::to_value(value).map_err(|source| StateError::Serialize {
            key: key.clone(),
            source,
        })?;
        self.put_value(key, v, StateMutationOrigin::Set);
        Ok(())
    }

    /// Infallible internal write of an already-serialized [`Value`].
    ///
    /// Shared by `set` and the value-level helpers (`merge`/`pick`/`rename`/
    /// `from_hashmap`) so those do not re-serialize and cannot fail.
    fn put_value(&self, key: String, v: Value, origin: StateMutationOrigin) {
        let old = self.get_raw(&key);
        if self.track_delta {
            self.delta.insert(key.clone(), DeltaOp::Put(v.clone()));
        } else {
            self.inner.insert(key.clone(), v.clone());
        }
        self.record_mutation(key, old, Some(v), origin);
    }

    /// Set a value directly in the committed store, bypassing delta tracking.
    ///
    /// Returns [`StateError`] if `value` cannot be serialized to JSON.
    pub fn set_committed(
        &self,
        key: impl Into<String>,
        value: impl serde::Serialize,
    ) -> Result<(), StateError> {
        let key = key.into();
        let v = serde_json::to_value(value).map_err(|source| StateError::Serialize {
            key: key.clone(),
            source,
        })?;
        let old = self.inner.insert(key.clone(), v.clone());
        self.record_mutation(key, old, Some(v), StateMutationOrigin::SetCommitted);
        Ok(())
    }

    /// Atomically read-modify-write a value under a per-key lock.
    ///
    /// If the key doesn't exist, `default` is used as the initial value. The
    /// function `f` receives the current value and returns the new value. The
    /// read-modify-write is performed while holding the map shard for `key`, so
    /// concurrent `modify` calls on the same key do not lose updates. Returns
    /// the new value, or [`StateError`] if it cannot be serialized.
    pub fn modify<T, F>(&self, key: &str, default: T, f: F) -> Result<T, StateError>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
        F: FnOnce(T) -> T,
    {
        use dashmap::mapref::entry::Entry;

        let serialize = |key: &str, val: &T| {
            serde_json::to_value(val).map_err(|source| StateError::Serialize {
                key: key.to_string(),
                source,
            })
        };

        if self.track_delta {
            // Atomic w.r.t. the delta shard; the committed base is read as the
            // initial value only when the delta has no entry for this key.
            match self.delta.entry(key.to_string()) {
                Entry::Occupied(mut o) => {
                    let current = match o.get() {
                        DeltaOp::Put(v) => serde_json::from_value(v.clone()).unwrap_or(default),
                        DeltaOp::Delete => default,
                    };
                    let old = self.inner.get(key).map(|r| r.value().clone());
                    let new_val = f(current);
                    let v = serialize(key, &new_val)?;
                    o.insert(DeltaOp::Put(v.clone()));
                    self.record_mutation(key.to_string(), old, Some(v), StateMutationOrigin::Set);
                    Ok(new_val)
                }
                Entry::Vacant(slot) => {
                    let base = self
                        .inner
                        .get(key)
                        .and_then(|r| serde_json::from_value(r.value().clone()).ok());
                    let old = self.inner.get(key).map(|r| r.value().clone());
                    let new_val = f(base.unwrap_or(default));
                    let v = serialize(key, &new_val)?;
                    slot.insert(DeltaOp::Put(v.clone()));
                    self.record_mutation(key.to_string(), old, Some(v), StateMutationOrigin::Set);
                    Ok(new_val)
                }
            }
        } else {
            match self.inner.entry(key.to_string()) {
                Entry::Occupied(mut o) => {
                    let old = o.get().clone();
                    let current = serde_json::from_value(old.clone()).unwrap_or(default);
                    let new_val = f(current);
                    let v = serialize(key, &new_val)?;
                    o.insert(v.clone());
                    self.record_mutation(
                        key.to_string(),
                        Some(old),
                        Some(v),
                        StateMutationOrigin::Set,
                    );
                    Ok(new_val)
                }
                Entry::Vacant(slot) => {
                    let new_val = f(default);
                    let v = serialize(key, &new_val)?;
                    slot.insert(v.clone());
                    self.record_mutation(key.to_string(), None, Some(v), StateMutationOrigin::Set);
                    Ok(new_val)
                }
            }
        }
    }

    /// Check if a key exists (in delta or inner).
    ///
    /// Applies the same transparent `derived:` fallback as [`Self::get`],
    /// [`Self::get_raw`] and [`Self::with`]: an unprefixed key also matches the
    /// computed variable `derived:{key}`. Flow predicates (`is_set`, `captured`)
    /// evaluate through this method, so without the fallback a computed value
    /// would read as permanently unknown while `get` returned it fine.
    pub fn contains(&self, key: &str) -> bool {
        if self.track_delta {
            match self.delta.get(key).map(|r| r.value().clone()) {
                Some(DeltaOp::Put(_)) => return true,
                Some(DeltaOp::Delete) => return false, // tombstone shadows inner
                None => {}
            }
        }
        if self.inner.contains_key(key) {
            return true;
        }
        if !key.contains(':') {
            let derived_key = format!("derived:{key}");
            if self.track_delta {
                match self.delta.get(&derived_key).map(|r| r.value().clone()) {
                    Some(DeltaOp::Put(_)) => return true,
                    Some(DeltaOp::Delete) => return false,
                    None => {}
                }
            }
            return self.inner.contains_key(&derived_key);
        }
        false
    }

    /// Remove a key.
    ///
    /// In delta-tracking mode this records a tombstone in the delta view and
    /// leaves the committed store untouched, so a subsequent `rollback()` fully
    /// restores the base state. Returns the value that was visible before removal.
    pub fn remove(&self, key: &str) -> Option<Value> {
        if self.track_delta {
            let removed = self.get_raw(key);
            // Tombstone in the delta — never mutate `inner` directly, so rollback
            // can restore the committed value.
            self.delta.insert(key.to_string(), DeltaOp::Delete);
            if let Some(ref old) = removed {
                self.record_mutation(
                    key.to_string(),
                    Some(old.clone()),
                    None,
                    StateMutationOrigin::Remove,
                );
            }
            removed
        } else {
            let removed = self.inner.remove(key).map(|(_, v)| v);
            if let Some(ref old) = removed {
                self.record_mutation(
                    key.to_string(),
                    Some(old.clone()),
                    None,
                    StateMutationOrigin::Remove,
                );
            }
            removed
        }
    }

    /// Get all keys (from both inner and delta when tracking).
    ///
    /// Keys tombstoned in the delta are excluded.
    pub fn keys(&self) -> Vec<String> {
        if !self.track_delta || self.delta.is_empty() {
            return self.inner.iter().map(|r| r.key().clone()).collect();
        }
        let mut seen =
            std::collections::HashSet::with_capacity(self.inner.len() + self.delta.len());
        let mut keys = Vec::with_capacity(self.inner.len() + self.delta.len());
        // Delta first so tombstones win over committed entries.
        for entry in self.delta.iter() {
            let key = entry.key().clone();
            seen.insert(key.clone());
            if matches!(entry.value(), DeltaOp::Put(_)) {
                keys.push(key);
            }
        }
        for entry in self.inner.iter() {
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
                new.put_value((*key).to_string(), v, StateMutationOrigin::Set);
            }
        }
        new
    }

    /// Merge another state into this one (other's values overwrite on conflict).
    pub fn merge(&self, other: &State) {
        for entry in other.inner.iter() {
            self.put_value(
                entry.key().clone(),
                entry.value().clone(),
                StateMutationOrigin::Set,
            );
        }
    }

    /// Rename a key.
    pub fn rename(&self, from: &str, to: &str) {
        if let Some(v) = self.remove(from) {
            self.put_value(to.to_string(), v, StateMutationOrigin::Set);
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

    /// Get a snapshot of the current delta's pending writes (tombstones omitted).
    pub fn delta(&self) -> HashMap<String, Value> {
        self.delta
            .iter()
            .filter_map(|entry| match entry.value() {
                DeltaOp::Put(v) => Some((entry.key().clone(), v.clone())),
                DeltaOp::Delete => None,
            })
            .collect()
    }

    /// Commit delta changes into the inner store, then clear the delta.
    ///
    /// Pending puts are applied and tombstones remove the committed key, so a
    /// removal made under delta tracking becomes durable only at commit time.
    pub fn commit(&self) {
        // Snapshot first so we don't iterate the delta while mutating `inner`.
        let ops: Vec<(String, DeltaOp)> = self
            .delta
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        for (key, op) in ops {
            match op {
                DeltaOp::Put(value) => {
                    let old = self.inner.insert(key.clone(), value.clone());
                    self.record_mutation_with_delta(
                        key,
                        old,
                        Some(value),
                        StateMutationOrigin::Commit,
                        false,
                    );
                }
                DeltaOp::Delete => {
                    if let Some((_, old)) = self.inner.remove(&key) {
                        self.record_mutation_with_delta(
                            key,
                            Some(old),
                            None,
                            StateMutationOrigin::Commit,
                            false,
                        );
                    }
                }
            }
        }
        self.delta.clear();
    }

    /// Discard all uncommitted delta changes, restoring the committed base state.
    ///
    /// Because removals and prefix clears under delta tracking only write
    /// tombstones (never mutating `inner`), dropping the delta is sufficient to
    /// restore the base — including keys that were removed in the transaction.
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
            // Values are already `Value`, so this write cannot fail to serialize.
            let old = self.inner.insert(key.clone(), value.clone());
            self.record_mutation(key, old, Some(value), StateMutationOrigin::SetCommitted);
        }
    }

    /// Remove all keys with the given prefix.
    ///
    /// In delta-tracking mode this writes tombstones for matching keys (from both
    /// the committed store and pending delta puts) without mutating the committed
    /// store, so `rollback()` restores everything that was cleared.
    pub fn clear_prefix(&self, prefix: &str) {
        if self.track_delta {
            let keys: Vec<String> = self
                .keys()
                .into_iter()
                .filter(|k| k.starts_with(prefix))
                .collect();
            for key in keys {
                let old = self.get_raw(&key);
                self.delta.insert(key.clone(), DeltaOp::Delete);
                if let Some(old) = old {
                    self.record_mutation(key, Some(old), None, StateMutationOrigin::ClearPrefix);
                }
            }
            return;
        }
        let keys_to_remove: Vec<String> = self
            .inner
            .iter()
            .filter(|entry| entry.key().starts_with(prefix))
            .map(|entry| entry.key().clone())
            .collect();
        for key in keys_to_remove {
            if let Some((_, old)) = self.inner.remove(&key) {
                self.record_mutation(key, Some(old), None, StateMutationOrigin::ClearPrefix);
            }
        }
    }

    /// Return a snapshot of recent state mutations.
    pub fn recent_mutations(&self) -> Vec<StateMutation> {
        self.mutations
            .lock()
            .expect("state mutation journal poisoned")
            .iter()
            .cloned()
            .collect()
    }

    /// Return the current monotonic cursor for the mutation journal.
    pub fn mutation_cursor(&self) -> u64 {
        self.next_mutation_sequence.load(Ordering::Relaxed) - 1
    }

    /// Return mutations appended after a previously captured cursor.
    pub fn mutations_since(&self, cursor: u64) -> Vec<StateMutation> {
        let mutations = self
            .mutations
            .lock()
            .expect("state mutation journal poisoned");
        mutations
            .iter()
            .filter(|mutation| mutation.sequence > cursor)
            .cloned()
            .collect()
    }

    /// Drain and return all recorded state mutations.
    pub fn drain_mutations(&self) -> Vec<StateMutation> {
        self.mutations
            .lock()
            .expect("state mutation journal poisoned")
            .drain(..)
            .collect()
    }

    /// Aggregate the [`SlotEvidence`] for a key: its current value, provenance
    /// (`state_meta:{key}`), confidence, and most-recent journal write.
    pub fn evidence(&self, key: &str) -> SlotEvidence {
        let value = self.get_raw(key);
        let meta = self.get::<Value>(&format!("state_meta:{key}"));
        let source = meta
            .as_ref()
            .and_then(|m| m.get("source"))
            .and_then(|s| s.as_str().map(String::from));
        let confidence = meta
            .as_ref()
            .and_then(|m| m.get("confidence"))
            .and_then(serde_json::Value::as_f64);

        let mut last_sequence: Option<u64> = None;
        let mut last_origin: Option<StateMutationOrigin> = None;
        for m in self.recent_mutations() {
            if m.key == key && last_sequence.is_none_or(|s| m.sequence > s) {
                last_sequence = Some(m.sequence);
                last_origin = Some(m.origin);
            }
        }

        SlotEvidence {
            key: key.to_string(),
            present: value.is_some(),
            value,
            source,
            confidence,
            last_sequence,
            last_origin,
        }
    }

    fn record_mutation(
        &self,
        key: String,
        old: Option<Value>,
        new: Option<Value>,
        origin: StateMutationOrigin,
    ) {
        self.record_mutation_with_delta(key, old, new, origin, self.track_delta);
    }

    fn record_mutation_with_delta(
        &self,
        key: String,
        old: Option<Value>,
        new: Option<Value>,
        origin: StateMutationOrigin,
        delta: bool,
    ) {
        let mut mutations = self
            .mutations
            .lock()
            .expect("state mutation journal poisoned");
        if mutations.len() >= self.mutation_capacity {
            mutations.pop_front();
        }
        let sequence = self.next_mutation_sequence.fetch_add(1, Ordering::Relaxed);
        let mutation = StateMutation {
            sequence,
            key,
            old,
            new,
            origin,
            timestamp: SystemTime::now(),
            delta,
        };
        // Durable sink runs under the journal lock so the file order matches
        // the ring order exactly. Sinks are sync + cheap by contract.
        if let Some(sink) = self.journal_sink.0.read().as_ref() {
            sink.write(&mutation);
        }
        mutations.push_back(mutation);
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
    ///
    /// Returns [`StateError`] if `value` cannot be serialized to JSON.
    pub fn set(
        &self,
        key: impl AsRef<str>,
        value: impl serde::Serialize,
    ) -> Result<(), StateError> {
        self.state.set(self.prefixed_key(key.as_ref()), value)
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
            .filter_map(|k| {
                k.strip_prefix(self.prefix)
                    .map(std::string::ToString::to_string)
            })
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
            .filter_map(|k| {
                k.strip_prefix(self.prefix)
                    .map(std::string::ToString::to_string)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_sink_receives_every_mutation_in_ring_order() {
        let state = State::new();
        let sink = Arc::new(MemoryJournalSink::new());
        state.set_journal_sink(sink.clone());

        let _ = state.set("a", 1);
        let _ = state.set("b", "two");
        state.remove("a");

        let entries = sink.entries();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries, state.recent_mutations());
        assert_eq!(entries[0].key, "a");
        assert_eq!(entries[2].origin, StateMutationOrigin::Remove);
    }

    #[test]
    fn journal_sink_is_shared_with_clones_and_delta_views() {
        let state = State::new();
        let sink = Arc::new(MemoryJournalSink::new());
        state.set_journal_sink(sink.clone());

        let clone = state.clone();
        let _ = clone.set("from_clone", true);

        let tracked = state.with_delta_tracking();
        let _ = tracked.set("from_delta", 1);
        tracked.commit();

        let keys: Vec<_> = sink.entries().iter().map(|m| m.key.clone()).collect();
        assert!(keys.contains(&"from_clone".to_string()));
        assert!(keys.contains(&"from_delta".to_string()));
        // Commit re-records the delta write into the committed store.
        assert!(
            sink.entries()
                .iter()
                .any(|m| m.origin == StateMutationOrigin::Commit)
        );
    }

    #[test]
    fn journal_sink_outlives_ring_capacity() {
        // The ring is bounded; the sink is not.
        let state = State::new();
        let sink = Arc::new(MemoryJournalSink::new());
        state.set_journal_sink(sink.clone());

        for i in 0..(DEFAULT_MUTATION_JOURNAL_CAPACITY + 10) {
            let _ = state.set(format!("k{i}"), i);
        }

        assert_eq!(
            state.recent_mutations().len(),
            DEFAULT_MUTATION_JOURNAL_CAPACITY
        );
        assert_eq!(sink.len(), DEFAULT_MUTATION_JOURNAL_CAPACITY + 10);
        assert_eq!(sink.entries()[0].key, "k0");
    }

    #[test]
    fn state_mutation_serde_round_trip_uses_epoch_millis() {
        let m = StateMutation {
            sequence: 42,
            key: "app:last_city".into(),
            old: None,
            new: Some(serde_json::json!("London")),
            origin: StateMutationOrigin::Set,
            timestamp: std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_718_000_000_123),
            delta: false,
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"timestamp_ms\":1718000000123"));
        assert!(json.contains("\"origin\":\"set\""));
        let back: StateMutation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn file_journal_sink_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "gemini-rs-journal-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.journal.jsonl");

        let state = State::new();
        {
            let sink = Arc::new(FileJournalSink::create(&path).unwrap());
            state.set_journal_sink(sink);
            let _ = state.set("a", 1);
            let _ = state.set("a", 2);
            state.remove("a");
            // Replace the sink so the file sink drops (and flushes).
            state.set_journal_sink(Arc::new(MemoryJournalSink::new()));
        }

        let data = std::fs::read_to_string(&path).unwrap();
        let parsed: Vec<StateMutation> = data
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].new, Some(serde_json::json!(1)));
        assert_eq!(parsed[2].origin, StateMutationOrigin::Remove);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_and_get_string() {
        let state = State::new();
        let _ = state.set("name", "Alice");
        assert_eq!(state.get::<String>("name"), Some("Alice".to_string()));
    }

    #[test]
    fn set_and_get_json() {
        let state = State::new();
        let _ = state.set("data", serde_json::json!({"temp": 22}));
        let v: Value = state.get("data").unwrap();
        assert_eq!(v["temp"], 22);
    }

    #[test]
    fn pick_subset() {
        let state = State::new();
        let _ = state.set("a", 1);
        let _ = state.set("b", 2);
        let _ = state.set("c", 3);
        let picked = state.pick(&["a", "c"]);
        assert!(picked.contains("a"));
        assert!(!picked.contains("b"));
        assert!(picked.contains("c"));
    }

    #[test]
    fn merge_states() {
        let s1 = State::new();
        let _ = s1.set("a", 1);
        let s2 = State::new();
        let _ = s2.set("b", 2);
        s1.merge(&s2);
        assert!(s1.contains("a"));
        assert!(s1.contains("b"));
    }

    #[test]
    fn rename_key() {
        let state = State::new();
        let _ = state.set("old", "value");
        state.rename("old", "new");
        assert!(!state.contains("old"));
        assert_eq!(state.get::<String>("new"), Some("value".to_string()));
    }

    #[test]
    fn remove_returns_value() {
        let state = State::new();
        let _ = state.set("key", 42);
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
        let _ = state.set("committed", "yes");

        let tracked = state.with_delta_tracking();
        let _ = tracked.set("new_key", "new_value");

        // New key visible through tracked state
        assert_eq!(
            tracked.get::<String>("new_key"),
            Some("new_value".to_string())
        );
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

        let _ = tracked.set("key", "val");
        assert!(tracked.has_delta());
    }

    #[test]
    fn delta_commit_merges_to_inner() {
        let state = State::new();
        let tracked = state.with_delta_tracking();
        let _ = tracked.set("key", "val");
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
        let _ = tracked.set("key", "val");
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
        let _ = tracked.set("a", 1);
        let _ = tracked.set("b", 2);

        let snapshot = tracked.delta();
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot.contains_key("a"));
        assert!(snapshot.contains_key("b"));
    }

    #[test]
    fn set_committed_bypasses_delta() {
        let state = State::new();
        let tracked = state.with_delta_tracking();
        let _ = tracked.set_committed("direct", "value");

        // Visible immediately in inner
        assert_eq!(state.get::<String>("direct"), Some("value".to_string()));
        // Not in delta
        assert!(!tracked.has_delta());
        // Still visible through tracked (reads inner too)
        assert_eq!(tracked.get::<String>("direct"), Some("value".to_string()));
    }

    #[test]
    fn mutation_journal_records_set_and_remove() {
        let state = State::new();
        let _ = state.set("key", "first");
        let _ = state.set("key", "second");
        state.remove("key");

        let mutations = state.recent_mutations();
        assert_eq!(mutations.len(), 3);
        assert_eq!(mutations[0].key, "key");
        assert_eq!(mutations[0].old, None);
        assert_eq!(mutations[0].new, Some(serde_json::json!("first")));
        assert_eq!(mutations[0].origin, StateMutationOrigin::Set);

        assert_eq!(mutations[1].old, Some(serde_json::json!("first")));
        assert_eq!(mutations[1].new, Some(serde_json::json!("second")));

        assert_eq!(mutations[2].old, Some(serde_json::json!("second")));
        assert_eq!(mutations[2].new, None);
        assert_eq!(mutations[2].origin, StateMutationOrigin::Remove);
    }

    #[test]
    fn mutation_journal_is_shared_with_delta_tracking() {
        let state = State::new();
        let _ = state.set("committed", "yes");

        let tracked = state.with_delta_tracking();
        let _ = tracked.set("committed", "maybe");
        tracked.commit();

        let mutations = state.recent_mutations();
        assert_eq!(mutations.len(), 3);
        assert_eq!(mutations[1].key, "committed");
        assert_eq!(mutations[1].old, Some(serde_json::json!("yes")));
        assert_eq!(mutations[1].new, Some(serde_json::json!("maybe")));
        assert_eq!(mutations[1].origin, StateMutationOrigin::Set);
        assert!(mutations[1].delta);

        assert_eq!(mutations[2].origin, StateMutationOrigin::Commit);
        assert!(!mutations[2].delta);
    }

    #[test]
    fn drain_mutations_clears_journal() {
        let state = State::new();
        let _ = state.set("a", 1);
        let _ = state.set("b", 2);

        let drained = state.drain_mutations();
        assert_eq!(drained.len(), 2);
        assert!(state.recent_mutations().is_empty());
    }

    #[test]
    fn mutation_cursor_reads_only_later_changes() {
        let state = State::new();
        let _ = state.set("before", 1);
        let cursor = state.mutation_cursor();

        let _ = state.set("after", 2);
        state.remove("before");

        let mutations = state.mutations_since(cursor);
        assert_eq!(mutations.len(), 2);
        assert_eq!(mutations[0].key, "after");
        assert_eq!(mutations[1].key, "before");
    }

    #[test]
    fn no_delta_tracking_preserves_existing_behavior() {
        let state = State::new();
        assert!(!state.is_tracking_delta());
        let _ = state.set("key", "val");
        assert_eq!(state.get::<String>("key"), Some("val".to_string()));
        assert!(!state.has_delta());
    }

    // ── Prefix tests ──────────────────────────────────────────────────────

    #[test]
    fn prefix_app_set_and_get() {
        let state = State::new();
        let _ = state.app().set("flag", true);

        // Accessible via prefix accessor
        assert_eq!(state.app().get::<bool>("flag"), Some(true));
        // Also accessible via raw key
        assert_eq!(state.get::<bool>("app:flag"), Some(true));
    }

    #[test]
    fn prefix_user_set_and_get() {
        let state = State::new();
        let _ = state.user().set("name", "Alice");
        assert_eq!(
            state.user().get::<String>("name"),
            Some("Alice".to_string())
        );
        assert_eq!(state.get::<String>("user:name"), Some("Alice".to_string()));
    }

    #[test]
    fn prefix_temp_set_and_get() {
        let state = State::new();
        let _ = state.temp().set("scratch", 42);
        assert_eq!(state.temp().get::<i32>("scratch"), Some(42));
    }

    #[test]
    fn prefix_contains_and_remove() {
        let state = State::new();
        let _ = state.app().set("x", 1);
        assert!(state.app().contains("x"));
        state.app().remove("x");
        assert!(!state.app().contains("x"));
    }

    #[test]
    fn prefix_keys() {
        let state = State::new();
        let _ = state.app().set("a", 1);
        let _ = state.app().set("b", 2);
        let _ = state.user().set("c", 3);

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
        let _ = tracked.app().set("flag", true);

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
        let _ = state.session().set("turn_count", 5);
        assert_eq!(state.session().get::<i32>("turn_count"), Some(5));
        assert_eq!(state.get::<i32>("session:turn_count"), Some(5));
    }

    #[test]
    fn prefix_turn_set_and_get() {
        let state = State::new();
        let _ = state.turn().set("transcript", "hello");
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
        let _ = state.bg().set("task_id", "abc-123");
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
        let _ = state.session().set("x", 1);
        assert!(state.session().contains("x"));
        state.session().remove("x");
        assert!(!state.session().contains("x"));
    }

    #[test]
    fn prefix_turn_keys() {
        let state = State::new();
        let _ = state.turn().set("a", 1);
        let _ = state.turn().set("b", 2);
        let _ = state.session().set("c", 3);

        let turn_keys = state.turn().keys();
        assert_eq!(turn_keys.len(), 2);
        assert!(turn_keys.contains(&"a".to_string()));
        assert!(turn_keys.contains(&"b".to_string()));
    }

    // ── try_get / try_get_key ─────────────────────────────────────────

    #[test]
    fn try_get_distinguishes_absent_from_wrong_type() {
        let state = State::new();
        assert!(matches!(state.try_get::<u32>("missing"), Ok(None)));

        state.set("n", 5u32).unwrap();
        assert_eq!(state.try_get::<u32>("n").unwrap(), Some(5));

        state.set("s", "not a number").unwrap();
        // Lenient read folds the type error into `None`…
        assert_eq!(state.get::<u32>("s"), None);
        // …the strict read reports it.
        match state.try_get::<u32>("s") {
            Err(StateError::WrongType { key, .. }) => assert_eq!(key, "s"),
            other => panic!("expected WrongType, got {other:?}"),
        }

        // Same derived: fallback as `get`.
        state.set("derived:risk", 0.5f64).unwrap();
        assert_eq!(state.try_get::<f64>("risk").unwrap(), Some(0.5));

        const N: StateKey<u32> = StateKey::new("n");
        assert_eq!(state.try_get_key(&N).unwrap(), Some(5));
        const S: StateKey<u32> = StateKey::new("s");
        assert!(state.try_get_key(&S).is_err());
    }

    // ── ReadOnlyPrefixedState (derived) tests ────────────────────────────

    #[test]
    fn derived_read_only_get() {
        let state = State::new();
        // Write via raw key (simulating ComputedRegistry)
        let _ = state.set("derived:sentiment", "positive");
        assert_eq!(
            state.derived().get::<String>("sentiment"),
            Some("positive".to_string())
        );
    }

    #[test]
    fn derived_read_only_get_raw() {
        let state = State::new();
        let _ = state.set("derived:score", serde_json::json!(0.95));
        let raw = state.derived().get_raw("score");
        assert!(raw.is_some());
        assert_eq!(raw.unwrap(), serde_json::json!(0.95));
    }

    #[test]
    fn derived_read_only_contains() {
        let state = State::new();
        let _ = state.set("derived:exists", true);
        assert!(state.derived().contains("exists"));
        assert!(!state.derived().contains("missing"));
    }

    #[test]
    fn derived_read_only_keys() {
        let state = State::new();
        let _ = state.set("derived:a", 1);
        let _ = state.set("derived:b", 2);
        let _ = state.set("app:c", 3);

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
        let _ = state.set("a", 1);
        let _ = state.set("b", "hello");
        let _ = state.set("c", true);

        let snap = state.snapshot_values(&["a", "b", "missing"]);
        assert_eq!(snap.len(), 2);
        assert_eq!(snap.get("a"), Some(&serde_json::json!(1)));
        assert_eq!(snap.get("b"), Some(&serde_json::json!("hello")));
        assert!(!snap.contains_key("missing"));
    }

    #[test]
    fn snapshot_values_empty_keys() {
        let state = State::new();
        let _ = state.set("a", 1);
        let snap = state.snapshot_values(&[]);
        assert!(snap.is_empty());
    }

    // ── diff_values tests ────────────────────────────────────────────────

    #[test]
    fn diff_values_detects_changed_value() {
        let state = State::new();
        let _ = state.set("x", 1);
        let snap = state.snapshot_values(&["x"]);

        let _ = state.set("x", 2);
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

        let _ = state.set("y", "new");
        let diffs = state.diff_values(&snap, &["y"]);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].0, "y");
        assert_eq!(diffs[0].1, Value::Null);
        assert_eq!(diffs[0].2, serde_json::json!("new"));
    }

    #[test]
    fn diff_values_detects_removed_key() {
        let state = State::new();
        let _ = state.set("z", 42);
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
        let _ = state.set("stable", 10);
        let snap = state.snapshot_values(&["stable"]);

        // No mutation
        let diffs = state.diff_values(&snap, &["stable"]);
        assert!(diffs.is_empty());
    }

    #[test]
    fn diff_values_multiple_keys_mixed_changes() {
        let state = State::new();
        let _ = state.set("a", 1);
        let _ = state.set("b", 2);
        let snap = state.snapshot_values(&["a", "b", "c"]);

        let _ = state.set("a", 10); // changed
        // b unchanged
        let _ = state.set("c", 3); // new

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
        let _ = state.set("turn:a", 1);
        let _ = state.set("turn:b", 2);
        let _ = state.set("app:c", 3);
        let _ = state.set("session:d", 4);

        state.clear_prefix("turn:");
        assert!(!state.contains("turn:a"));
        assert!(!state.contains("turn:b"));
        assert!(state.contains("app:c"));
        assert!(state.contains("session:d"));
    }

    #[test]
    fn clear_prefix_no_matching_keys_is_noop() {
        let state = State::new();
        let _ = state.set("app:x", 1);
        state.clear_prefix("turn:");
        assert!(state.contains("app:x"));
    }

    #[test]
    fn clear_prefix_also_clears_delta() {
        let state = State::new();
        let _ = state.set("turn:committed", 1);
        let tracked = state.with_delta_tracking();
        let _ = tracked.set("turn:delta_val", 2);

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
        let _ = state.turn().set("x", 1);
        let _ = state.turn().set("y", 2);
        let _ = state.app().set("z", 3);

        state.clear_prefix("turn:");
        assert!(state.turn().keys().is_empty());
        assert!(state.app().contains("z"));
    }

    // ── modify() tests ──────────────────────────────────────────────────

    #[test]
    fn modify_increment_existing() {
        let state = State::new();
        let _ = state.set("count", 5u32);
        let result = state.modify("count", 0u32, |n| n + 1).unwrap();
        assert_eq!(result, 6);
        assert_eq!(state.get::<u32>("count"), Some(6));
    }

    #[test]
    fn modify_uses_default_when_missing() {
        let state = State::new();
        let result = state.modify("new_count", 0u32, |n| n + 1).unwrap();
        assert_eq!(result, 1);
        assert_eq!(state.get::<u32>("new_count"), Some(1));
    }

    #[test]
    fn modify_with_delta_tracking() {
        let state = State::new();
        let _ = state.set("x", 10u32);
        let tracked = state.with_delta_tracking();
        let result = tracked.modify("x", 0u32, |n| n * 2).unwrap();
        assert_eq!(result, 20);
        // Written to delta, not committed
        assert_eq!(tracked.get::<u32>("x"), Some(20));
        assert_eq!(state.get::<u32>("x"), Some(10)); // original unchanged
    }

    // ── derived fallback tests ──────────────────────────────────────────

    #[test]
    fn get_falls_back_to_derived_prefix() {
        let state = State::new();
        let _ = state.set("derived:risk", 0.85);
        // Access without prefix — should find derived:risk
        assert_eq!(state.get::<f64>("risk"), Some(0.85));
    }

    #[test]
    fn get_prefers_direct_key_over_derived() {
        let state = State::new();
        let _ = state.set("score", 1.0);
        let _ = state.set("derived:score", 0.5);
        // Direct key should win
        assert_eq!(state.get::<f64>("score"), Some(1.0));
    }

    #[test]
    fn get_derived_fallback_skipped_for_prefixed_keys() {
        let state = State::new();
        let _ = state.set("derived:risk", 0.85);
        // Prefixed key should NOT trigger fallback
        assert_eq!(state.get::<f64>("app:risk"), None);
    }

    #[test]
    fn get_derived_fallback_with_delta_tracking() {
        let state = State::new();
        let tracked = state.with_delta_tracking();
        let _ = tracked.set("derived:computed_val", 42);
        assert_eq!(tracked.get::<i32>("computed_val"), Some(42));
    }

    // ── with() zero-copy borrow tests ──────────────────────────────────

    #[test]
    fn with_reads_from_inner() {
        let state = State::new();
        let _ = state.set("name", "Alice");
        let len = state.with("name", |v| v.as_str().unwrap().len());
        assert_eq!(len, Some(5));
    }

    #[test]
    fn with_reads_from_delta_first() {
        let state = State::new();
        let _ = state.set("x", 1);
        let tracked = state.with_delta_tracking();
        let _ = tracked.set("x", 99);
        let val = tracked.with("x", |v| v.as_i64().unwrap());
        assert_eq!(val, Some(99));
    }

    #[test]
    fn with_falls_back_to_inner_when_not_in_delta() {
        let state = State::new();
        let _ = state.set("committed", "yes");
        let tracked = state.with_delta_tracking();
        let val = tracked.with("committed", |v| v.as_str().unwrap().to_string());
        assert_eq!(val, Some("yes".to_string()));
    }

    #[test]
    fn with_falls_back_to_derived() {
        let state = State::new();
        let _ = state.set("derived:risk", 0.85);
        let val = state.with("risk", |v| v.as_f64().unwrap());
        assert_eq!(val, Some(0.85));
    }

    #[test]
    fn with_derived_fallback_skipped_for_prefixed() {
        let state = State::new();
        let _ = state.set("derived:risk", 0.85);
        let val = state.with("app:risk", |v| v.as_f64().unwrap());
        assert_eq!(val, None);
    }

    #[test]
    fn with_returns_none_for_missing() {
        let state = State::new();
        let val = state.with("missing", std::clone::Clone::clone);
        assert_eq!(val, None);
    }

    #[test]
    fn with_on_prefixed_state() {
        let state = State::new();
        let _ = state.app().set("flag", true);
        let val = state.app().with("flag", |v| v.as_bool().unwrap());
        assert_eq!(val, Some(true));
    }

    #[test]
    fn with_on_read_only_prefixed_state() {
        let state = State::new();
        let _ = state.set("derived:score", serde_json::json!(0.95));
        let val = state.derived().with("score", |v| v.as_f64().unwrap());
        assert_eq!(val, Some(0.95));
    }

    // ── StateKey typed key tests ───────────────────────────────────────

    const TURN_COUNT: StateKey<u32> = StateKey::new("session:turn_count");
    const NAME: StateKey<String> = StateKey::new("user:name");

    #[test]
    fn state_key_get_and_set() {
        let state = State::new();
        let _ = state.set_key(&TURN_COUNT, 5);
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
        let _ = state.set_key(&NAME, "Alice".to_string());
        assert_eq!(state.get_key(&NAME), Some("Alice".to_string()));
    }

    #[test]
    fn state_key_with() {
        let state = State::new();
        let _ = state.set_key(&TURN_COUNT, 42);
        let val = state.with_key(&TURN_COUNT, |v| v.as_u64().unwrap());
        assert_eq!(val, Some(42));
    }

    #[test]
    fn state_key_interop_with_raw() {
        let state = State::new();
        let _ = state.set_key(&TURN_COUNT, 10);
        // Can also read via raw key
        assert_eq!(state.get::<u32>("session:turn_count"), Some(10));
    }

    #[test]
    fn slot_evidence_aggregates_value_provenance_and_journal() {
        let state = State::new();
        let _ = state.set("party_size", 6u8);
        // Provenance written under the state_meta convention (as resolvers do).
        let _ = state.set(
            "state_meta:party_size",
            serde_json::json!({ "source": "extraction", "confidence": 0.9 }),
        );

        let ev = state.evidence("party_size");
        assert!(ev.present);
        assert_eq!(ev.value, Some(serde_json::json!(6)));
        assert_eq!(ev.source.as_deref(), Some("extraction"));
        assert_eq!(ev.confidence, Some(0.9));
        assert!(ev.last_sequence.is_some());
        assert_eq!(ev.last_origin, Some(StateMutationOrigin::Set));

        // An absent key reports no evidence.
        let missing = state.evidence("nope");
        assert!(!missing.present);
        assert!(missing.source.is_none());
    }

    // ── Transaction-invariant tests (the verified correctness bugs) ──────────

    #[test]
    fn rollback_restores_base_after_remove() {
        // Regression: previously remove() in delta mode deleted from the committed
        // store, so rollback() could not restore it.
        let base = State::new();
        let _ = base.set("k", "original");

        let tx = base.with_delta_tracking();
        assert_eq!(tx.remove("k"), Some(serde_json::json!("original")));
        assert_eq!(tx.get::<String>("k"), None); // tombstoned in the tx view
        assert_eq!(base.get::<String>("k"), Some("original".into())); // base intact

        tx.rollback();
        assert_eq!(tx.get::<String>("k"), Some("original".into()));
        assert_eq!(base.get::<String>("k"), Some("original".into()));
    }

    #[test]
    fn rollback_restores_base_after_clear_prefix() {
        // Regression: clear_prefix() used to mutate the committed store directly.
        let base = State::new();
        let _ = base.set("app:a", 1u32);
        let _ = base.set("app:b", 2u32);
        let _ = base.set("user:c", 3u32);

        let tx = base.with_delta_tracking();
        tx.clear_prefix("app:");
        assert_eq!(tx.get::<u32>("app:a"), None);
        assert_eq!(tx.get::<u32>("app:b"), None);
        assert_eq!(tx.get::<u32>("user:c"), Some(3));
        // Base untouched until commit.
        assert_eq!(base.get::<u32>("app:a"), Some(1));

        tx.rollback();
        assert_eq!(tx.get::<u32>("app:a"), Some(1));
        assert_eq!(tx.get::<u32>("app:b"), Some(2));
    }

    #[test]
    fn commit_applies_removals() {
        let base = State::new();
        let _ = base.set("k", "v");
        let tx = base.with_delta_tracking();
        tx.remove("k");
        tx.commit();
        assert_eq!(base.get::<String>("k"), None);
    }

    #[test]
    fn commit_applies_prefix_clear() {
        let base = State::new();
        let _ = base.set("app:a", 1u32);
        let _ = base.set("user:c", 3u32);
        let tx = base.with_delta_tracking();
        tx.clear_prefix("app:");
        tx.commit();
        assert_eq!(base.get::<u32>("app:a"), None);
        assert_eq!(base.get::<u32>("user:c"), Some(3));
    }

    #[test]
    fn modify_is_atomic_under_concurrency() {
        use std::sync::Arc;
        use std::thread;

        let state = Arc::new(State::new());
        let _ = state.set("count", 0u64);

        let threads = 8;
        let per_thread = 1000;
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let state = state.clone();
                thread::spawn(move || {
                    for _ in 0..per_thread {
                        let _ = state.modify("count", 0u64, |n| n + 1);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        // With a real per-key atomic RMW, no increments are lost.
        assert_eq!(
            state.get::<u64>("count"),
            Some((threads * per_thread) as u64)
        );
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    // A transaction's puts and removes never leak to the base before commit, and
    // a rollback always restores the exact committed base.
    proptest! {
        #[test]
        fn rollback_always_restores_base(
            base_keys in proptest::collection::vec(("[a-c]", 0u32..5), 0..6),
            ops in proptest::collection::vec(
                prop_oneof![
                    ("[a-c]", 0u32..5).prop_map(|(k, v)| (k, Some(v))),
                    "[a-c]".prop_map(|k| (k, None)),
                ],
                0..12,
            ),
        ) {
            let base = State::new();
            for (k, v) in &base_keys {
                let _ = base.set(k.clone(), *v);
            }
            let snapshot = |s: &State| -> std::collections::BTreeMap<String, Value> {
                s.keys().into_iter().filter_map(|k| s.get_raw(&k).map(|v| (k, v))).collect()
            };
            let before = snapshot(&base);

            let tx = base.with_delta_tracking();
            for (k, v) in &ops {
                match v {
                    Some(v) => { let _ = tx.set(k.clone(), *v); }
                    None => { tx.remove(k); }
                }
            }
            // Base is never mutated while the tx is open.
            prop_assert_eq!(&before, &snapshot(&base));

            tx.rollback();
            prop_assert_eq!(&before, &snapshot(&tx));
        }
    }
}

#[cfg(test)]
mod derived_contains_fallback {
    //! `contains` must agree with `get` about the transparent `derived:`
    //! fallback. Flow predicates (`is_set`, `captured`) evaluate through
    //! `contains`, so a computed variable that `get` returns but `contains`
    //! denies reads as permanently unknown to the flow.
    use super::State;

    #[test]
    fn contains_sees_a_derived_value_through_the_unprefixed_key() {
        let state = State::new();
        state.set("derived:risk", 0.85).unwrap();
        assert_eq!(
            state.get::<f64>("risk"),
            Some(0.85),
            "precondition: get falls back"
        );
        assert!(
            state.contains("risk"),
            "contains must fall back the same way get does"
        );
    }

    #[test]
    fn contains_fallback_respects_delta_tracking_and_tombstones() {
        let state = State::new();
        state.set("derived:score", 1u32).unwrap();
        let tracked = state.with_delta_tracking();
        assert!(
            tracked.contains("score"),
            "inner derived value visible through tracked view"
        );
        tracked.remove("derived:score");
        assert!(
            !tracked.contains("score"),
            "a tombstone on the derived key shadows inner"
        );
    }

    #[test]
    fn contains_does_not_fall_back_for_prefixed_keys() {
        let state = State::new();
        state.set("derived:flag", true).unwrap();
        assert!(
            !state.contains("session:flag"),
            "only unprefixed keys get the fallback"
        );
    }
}
