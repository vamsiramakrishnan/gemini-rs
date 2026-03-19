# State Management

The `State` type is a shared, concurrent key-value store that flows through every
component of a live session: callbacks, tool calls, extractors, watchers, phases,
and computed variables. Values are stored as `serde_json::Value` and deserialized
on read, giving you type safety without a rigid schema.

## Reading and Writing

```rust,ignore
use gemini_adk_rs::State;

let state = State::new();

// Write any serializable value
state.set("customer_name", "Alice");
state.set("turn_count", 5u32);
state.set("scores", vec![0.8, 0.9, 0.7]);

// Read with type inference
let name: Option<String> = state.get("customer_name");
let count: Option<u32> = state.get("turn_count");

// Read with a default fallback
let count: u32 = state.get("turn_count").unwrap_or(0);

// Check existence
if state.contains("customer_name") {
    // ...
}

// Remove a key
let removed: Option<serde_json::Value> = state.remove("customer_name");
```

### Zero-Copy Reads

For hot paths where you want to avoid cloning, use `with()` to borrow the
underlying `Value` directly through the `DashMap` ref-guard:

```rust,ignore
// Borrow without cloning
let len = state.with("customer_name", |v| v.as_str().unwrap().len());

// Avoids: state.get_raw("key").map(|v| ...) which clones the Value
```

## Prefix Scoping

Every state key belongs to a namespace defined by its prefix. Prefixes establish
ownership, lifecycle, and read/write rules:

| Prefix      | Who writes            | Lifecycle              | Example keys                        |
|-------------|-----------------------|------------------------|-------------------------------------|
| `session:`  | SDK (SessionSignals)  | Entire session         | `session:is_user_speaking`          |
| `derived:`  | SDK (ComputedRegistry)| Recomputed each turn   | `derived:sentiment_score`           |
| `turn:`     | SDK / User code       | Cleared each turn      | `turn:transcript`                   |
| `app:`      | User code             | Entire session         | `app:order_total`                   |
| `user:`     | User code             | Entire session         | `user:name`                         |
| `bg:`       | Background tasks      | Entire session         | `bg:search_failed`                  |
| `temp:`     | User code             | No automatic lifecycle | `temp:scratch`                      |

Keys without a prefix (e.g. `"customer_name"`) are valid and commonly used for
extractor-populated fields. The prefix convention is for organizational clarity,
not enforcement -- the store does not reject unprefixed keys.

## Scoped Accessors

Each prefix has a corresponding accessor that automatically prepends the prefix.
This reduces typos and keeps code clean:

```rust,ignore
// These two are equivalent:
state.set("app:flag", true);
state.app().set("flag", true);

// Reading
let flag: Option<bool> = state.app().get("flag");

// Listing keys in a scope (prefix stripped from results)
let app_keys: Vec<String> = state.app().keys();
// Returns: ["flag"] not ["app:flag"]

// Other scoped accessors
state.session().set("turn_count", 5);
state.user().set("name", "Alice");
state.turn().set("transcript", "hello");
state.bg().set("task_id", "abc-123");
state.temp().set("scratch", 42);

// derived() is read-only -- no set() or remove()
let score: Option<f64> = state.derived().get("sentiment_score");
```

## Atomic Modify

When multiple components read-modify-write the same key, use `modify()` to avoid
lost updates. It reads the current value (or a default), applies your function,
and writes the result back:

```rust,ignore
// Increment a counter (uses 0 if key doesn't exist yet)
let new_count = state.modify("turn_count", 0u32, |n| n + 1);

// Toggle a boolean
state.modify("muted", false, |b| !b);

// Append to a running total
state.modify("app:total_score", 0.0f64, |total| total + new_score);
```

Note: `modify()` uses the same DashMap as `get`/`set`. It is atomic in the sense
that no other `modify` on the same key can interleave, but it is not a database
transaction.

## Derived Fallback

When you call `state.get("risk")` and the key `"risk"` does not exist, State
automatically checks `"derived:risk"` as a fallback. This means computed variables
are accessible without the prefix tax:

```rust,ignore
// ComputedRegistry writes to "derived:risk_level"
// You can read it either way:
let risk: Option<String> = state.get("derived:risk_level");
let risk: Option<String> = state.get("risk_level"); // same result

// Direct key wins if both exist:
state.set("score", 1.0);
state.set("derived:score", 0.5);
let score: f64 = state.get("score").unwrap(); // returns 1.0
```

The fallback only triggers for unprefixed keys. `state.get("app:risk")` will
never fall back to `"derived:risk"`.

## StateKey -- Type-Safe Keys

For keys used in multiple places, define a `StateKey<T>` constant to eliminate
string typos and enforce type consistency at compile time:

```rust,ignore
use gemini_adk_rs::state::StateKey;

const TURN_COUNT: StateKey<u32> = StateKey::new("session:turn_count");
const SENTIMENT: StateKey<f64> = StateKey::new("derived:sentiment_score");
const USER_NAME: StateKey<String> = StateKey::new("user:name");

// Usage
state.set_key(&TURN_COUNT, 5);
let count: Option<u32> = state.get_key(&TURN_COUNT);

// Zero-copy borrow with typed key
let val = state.with_key(&TURN_COUNT, |v| v.as_u64().unwrap());

// Interoperable with raw string access
assert_eq!(state.get::<u32>("session:turn_count"), Some(5));
```

## Delta Tracking

Delta tracking creates a transactional view of state. Writes go to a separate
delta map that can be committed or rolled back:

```rust,ignore
let state = State::new();
state.set("committed_key", "original");

// Create a delta-tracking view (shares the same backing store)
let tracked = state.with_delta_tracking();

// Writes go to delta, not to the committed store
tracked.set("new_key", "pending");
assert!(tracked.contains("new_key"));    // visible through tracked
assert!(!state.contains("new_key"));     // NOT visible in original

// Reads check delta first, then committed store
let val: String = tracked.get("committed_key").unwrap(); // reads from committed

// Commit: merges delta into the committed store
tracked.commit();
assert!(state.contains("new_key")); // now visible everywhere

// Or rollback: discards all pending changes
tracked.rollback();
```

Useful for extractor pipelines where you want to validate extracted data before
committing it to the shared state.

## State in Tool Calls

The `on_tool_call` callback receives `State` so you can promote tool results into
state keys that watchers and phase transitions react to:

```rust,ignore
Live::builder()
    .on_tool_call(|calls, state| async move {
        // Let the dispatcher handle execution, but promote results
        None // returning None means "auto-dispatch"
    })
    .before_tool_response(|responses, state| async move {
        // Inspect tool results and promote to state
        for r in &responses {
            if r.name == "verify_identity" {
                if r.response.get("verified") == Some(&json!(true)) {
                    state.set("identity_verified", true);
                }
            }
        }
        responses
    })
```

## Auto-Tracked Session State

`SessionSignals` automatically writes session-level signals to the `session:`
prefix. You never need to set these manually:

| Key                               | Type     | Updated on                  |
|-----------------------------------|----------|-----------------------------|
| `session:is_user_speaking`        | `bool`   | VoiceActivityStart/End      |
| `session:is_model_speaking`       | `bool`   | PhaseChanged(ModelSpeaking) |
| `session:interrupt_count`         | `u64`    | Each interruption           |
| `session:error_count`             | `u64`    | Each error event            |
| `session:last_error`              | `String` | Each error event            |
| `session:silence_ms`              | `u64`    | Periodic flush (~100ms)     |
| `session:elapsed_ms`              | `u64`    | Periodic flush (~100ms)     |
| `session:remaining_budget_ms`     | `u64`    | Periodic flush (~100ms)     |
| `session:go_away_received`        | `bool`   | GoAway from server          |
| `session:go_away_time_left_ms`    | `u64`    | GoAway with time left       |
| `session:resumable`               | `bool`   | SessionResumeHandle         |
| `session:total_token_count`       | `u32`    | Each UsageMetadata event    |
| `session:prompt_token_count`      | `u32`    | Each UsageMetadata event    |
| `session:response_token_count`    | `u32`    | Each UsageMetadata event    |
| `session:cached_content_token_count`| `u32`  | Each UsageMetadata event    |
| `session:thoughts_token_count`    | `u32`    | Each UsageMetadata event    |
| `session:last_input_transcription`| `String` | Each input transcription    |
| `session:last_output_transcription`| `String`| Each output transcription   |
| `session:phase`                   | `String` | PhaseChanged                |
| `session:session_type`            | `String` | Connected / mark_video_sent |
| `session:disconnected`            | `bool`   | Disconnected                |

Read them anywhere:

```rust,ignore
let speaking: bool = state.session().get("is_user_speaking").unwrap_or(false);
let elapsed: u64 = state.session().get("elapsed_ms").unwrap_or(0);
let budget: u64 = state.session().get("remaining_budget_ms").unwrap_or(0);
```

## Utility Methods

```rust,ignore
// Snapshot specific keys (for diffing later)
let snap = state.snapshot_values(&["score", "mood"]);

// Diff against a previous snapshot
state.set("score", 99);
let diffs = state.diff_values(&snap, &["score", "mood"]);
// diffs: [("score", old_value, new_value)]

// Pick a subset of keys into a new State
let subset = state.pick(&["name", "score"]);

// Merge another state in (overwrites on conflict)
state.merge(&other_state);

// Rename a key
state.rename("old_key", "new_key");

// Clear all keys with a given prefix
state.clear_prefix("turn:");
```
