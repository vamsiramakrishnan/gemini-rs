# Bridging the Gaps: The Delta

What a perf-obsessed Rust engineer would actually ship to close every gap in the current architecture.

Ordered by **blast radius × effort**. P0 ships this week. P1 ships next. P2 whenever.

---

## P0-1: Zero-Copy State Reads

### The Problem

Every `state.get::<T>(key)` does: DashMap lookup → `v.value().clone()` → `serde_json::from_value()`. That's a full JSON tree clone + deserialization per read. Guard closures do 1-3 reads each. 4 transitions × 3 reads = 12 clones per TurnComplete evaluation. For simple booleans. That's embarrassing.

### The Fix

Add `state.with()` that gives you a scoped reference to the raw Value without cloning:

```rust
// state.rs — new method
pub fn with<F, R>(&self, key: &str, f: F) -> Option<R>
where
    F: FnOnce(&Value) -> R,
{
    // Check delta first (if tracking)
    if self.track_delta {
        if let Some(ref_multi) = self.delta.get(key) {
            return Some(f(ref_multi.value()));
        }
    }
    // Then inner
    if let Some(ref_multi) = self.inner.get(key) {
        return Some(f(ref_multi.value()));
    }
    // Derived fallback
    if !key.contains(':') {
        let derived_key = format!("derived:{key}");
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
```

Now guard closures become zero-copy:

```rust
// Before: clones Value, deserializes to bool, drops both
S::is_true("verified")  // state.get::<bool>(key) — 1 clone + 1 deser

// After: borrows Value in-place, reads as_bool()
pub fn is_true(key: &str) -> impl Fn(&State) -> bool + Send + Sync + '_ {
    move |s: &State| s.with(key, |v| v.as_bool().unwrap_or(false)).unwrap_or(false)
}

pub fn eq<'a>(key: &'a str, expected: &'a str) -> impl Fn(&State) -> bool + Send + Sync + 'a {
    move |s: &State| {
        s.with(key, |v| v.as_str().map(|s| s == expected).unwrap_or(false))
            .unwrap_or(false)
    }
}
```

### Cost

- 4 hours implementation + tests
- Zero breaking changes — `get()` still works, `with()` is additive
- Guard evaluation goes from ~12 clones/turn → 0 clones/turn

### Files

- `crates/rs-adk/src/state.rs` — add `with()`, `with_raw()`
- `crates/adk-rs-fluent/src/compose/state.rs` — rewrite S predicates to use `with()`
- Update existing tests, add new benchmarks

---

## P0-2: TranscriptBuffer Ring Cap

### The Problem

`TranscriptBuffer::turns: Vec<TranscriptTurn>` grows forever. 30-minute call × turn every 5s = 360 turns. Each turn owns 2 Strings + Vec<ToolCallSummary>. Extractors only window the last 5-10, but the buffer keeps all 360.

### The Fix

Cap the buffer. Use VecDeque and drop old turns:

```rust
pub struct TranscriptBuffer {
    turns: VecDeque<TranscriptTurn>,  // was Vec
    max_turns: usize,                  // new
    current_user: String,
    current_model: String,
    tool_calls_pending: Vec<ToolCallSummary>,
    turn_count: u32,
}

impl TranscriptBuffer {
    pub fn new() -> Self {
        Self::with_capacity(100)  // default: keep last 100 turns
    }

    pub fn with_capacity(max_turns: usize) -> Self {
        Self {
            turns: VecDeque::with_capacity(max_turns),
            max_turns,
            ..
        }
    }

    pub fn end_turn(&mut self) -> Option<TranscriptTurn> {
        // ... existing logic to build TranscriptTurn ...
        if self.turns.len() >= self.max_turns {
            self.turns.pop_front();  // drop oldest
        }
        self.turns.push_back(turn);
        self.turns.back().cloned()
    }
}
```

### Cost

- 2 hours. Vec → VecDeque is a one-line type change. `window()` uses `.iter().rev().take(n)` instead of slice indexing.
- No API change. Callers already use `window(n)` which returns the last N.

### Files

- `crates/rs-adk/src/live/transcript.rs`
- `crates/rs-adk/src/live/builder.rs` — expose `.transcript_capacity(n)` on LiveSessionBuilder

---

## P0-3: Eliminate Contents Clone in LlmTextAgent

### The Problem

```rust
// text.rs line 161
let request = self.build_request(contents.clone());  // DEEP CLONE of entire conversation
```

Every tool-dispatch round clones the full conversation history. Round 10 clones rounds 1-9. For a 5-round agent with 4KB prompts, that's ~20KB of unnecessary allocations per run.

### The Fix

Pass contents by reference. `build_request` doesn't need ownership:

```rust
fn build_request(&self, contents: &[Content]) -> LlmRequest {
    let mut req = LlmRequest::from_contents(contents.to_vec()); // clone once, at the boundary
    req.system_instruction = self.instruction.clone();
    req.temperature = self.temperature;
    req.max_output_tokens = self.max_output_tokens;
    if let Some(dispatcher) = &self.dispatcher {
        req.tools = dispatcher.to_tool_declarations();
    }
    req
}
```

Wait — `LlmRequest::from_contents` takes `Vec<Content>`. And `BaseLlm::generate` takes `LlmRequest` by value. So the clone at the API boundary is unavoidable. But we can avoid the double-clone:

```rust
async fn run(&self, state: &State) -> Result<String, AgentError> {
    let input = state.get::<String>("input").unwrap_or_default();
    let mut contents = vec![Content::user(&input)];

    for _round in 0..MAX_TOOL_ROUNDS {
        let request = self.build_request(contents.clone()); // still need this
        let response = self.llm.generate(request).await?;

        let calls = response.function_calls();
        if calls.is_empty() {
            let text = response.text();
            state.set("output", &text);
            return Ok(text);
        }

        // Append model response — move, don't clone
        contents.push(response.content);  // was: response.content.clone()
        // ...
    }
}
```

The real fix is making `LlmResponse.content` consumable. Currently `function_calls()` borrows `&self.content`, so we can't move content out until after we've checked for function calls. Split the access:

```rust
// llm/mod.rs — add consuming method
impl LlmResponse {
    /// Check if there are function calls and consume the response.
    pub fn into_parts(self) -> (Content, Vec<FunctionCall>) {
        let calls: Vec<FunctionCall> = self.content.parts.iter()
            .filter_map(|p| match p {
                Part::FunctionCall { function_call } => Some(function_call.clone()),
                _ => None,
            })
            .collect();
        (self.content, calls)
    }
}

// text.rs — use consuming access
let response = self.llm.generate(request).await?;
let (content, calls) = response.into_parts();

if calls.is_empty() {
    // Extract text from content without clone
    let text = content.parts.iter()
        .filter_map(|p| match p { Part::Text { text } => Some(text.as_str()), _ => None })
        .collect::<Vec<_>>()
        .join("");
    state.set("output", &text);
    return Ok(text);
}

contents.push(content);  // MOVE, not clone
```

### Cost

- 3 hours. Eliminates 1 clone per round. For 5-round agents, saves 4 unnecessary deep clones of growing conversation.
- `into_parts()` is additive, doesn't break existing API.

### Files

- `crates/rs-adk/src/llm/mod.rs` — add `into_parts()`
- `crates/rs-adk/src/text.rs` — use consuming access pattern

---

## P0-4: VertexAI Token Refresh

### The Problem

`GeminiLlm::new()` reads `GOOGLE_ACCESS_TOKEN` once from env. Tokens expire after 3600 seconds. A long-running service dies silently after 1 hour.

### The Fix

Add a `TokenProvider` trait. Default impl shells out to `gcloud auth print-access-token` on demand with caching:

```rust
// llm/token.rs
#[async_trait]
pub trait TokenProvider: Send + Sync {
    async fn token(&self) -> Result<String, LlmError>;
}

/// Caches token, refreshes when expired (TTL-based).
pub struct GcloudTokenProvider {
    cached: tokio::sync::RwLock<Option<(String, Instant)>>,
    ttl: Duration,  // default: 3000s (50 min, well before 3600s expiry)
}

#[async_trait]
impl TokenProvider for GcloudTokenProvider {
    async fn token(&self) -> Result<String, LlmError> {
        // Check cache
        {
            let guard = self.cached.read().await;
            if let Some((ref token, issued_at)) = *guard {
                if issued_at.elapsed() < self.ttl {
                    return Ok(token.clone());
                }
            }
        }
        // Refresh
        let output = tokio::process::Command::new("gcloud")
            .args(["auth", "print-access-token"])
            .output()
            .await
            .map_err(|e| LlmError::RequestFailed(format!("gcloud auth failed: {e}")))?;

        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if token.is_empty() {
            return Err(LlmError::RequestFailed("Empty token from gcloud".into()));
        }

        let mut guard = self.cached.write().await;
        *guard = Some((token.clone(), Instant::now()));
        Ok(token)
    }
}

/// Static token (for testing or short-lived processes).
pub struct StaticTokenProvider(String);

#[async_trait]
impl TokenProvider for StaticTokenProvider {
    async fn token(&self) -> Result<String, LlmError> {
        Ok(self.0.clone())
    }
}
```

Wire into `GeminiLlm`:

```rust
pub struct GeminiLlm {
    // ...
    #[cfg(feature = "gemini-llm")]
    token_provider: Option<Arc<dyn TokenProvider>>,
}

// In generate():
if self.variant == GoogleLlmVariant::VertexAi {
    if let Some(provider) = &self.token_provider {
        let fresh_token = provider.token().await?;
        // Rebuild client with fresh token — OR better, set token on existing client
    }
}
```

**Better approach** — make `rs_genai::Client` accept a `TokenProvider` trait so it refreshes transparently:

```rust
// rs-genai/src/transport/auth.rs — extend VertexAIAuth
pub struct VertexAIAuth {
    project: String,
    location: String,
    token_provider: Arc<dyn TokenProvider>,  // was: String token
}
```

This way token refresh is handled at L0, invisible to L1/L2.

### Cost

- 6 hours. New trait + gcloud impl + caching + wire through auth layer.
- Fixes the "dies after 1 hour" production bug.

### Files

- `crates/rs-genai/src/transport/auth.rs` — `TokenProvider` trait, `GcloudTokenProvider`
- `crates/rs-adk/src/llm/gemini.rs` — accept `TokenProvider` in params
- `crates/rs-adk/src/llm/mod.rs` — re-export

---

## P1-1: Typed State Keys

### The Problem

```rust
.transition("verify", S::is_true("idenity_verified"))  // typo: "idenity"
// Silently returns false forever. Debug for 30 minutes.
```

State keys are `&str`. No compile-time check. No runtime warning when reading a key that was never written.

### The Fix

Const key registry with known-key validation:

```rust
// Approach: StateKey constants + optional runtime validation

/// Declare state keys as typed constants.
pub mod keys {
    use super::StateKey;

    pub const EMOTIONAL_STATE: StateKey<String> = StateKey::new("emotional_state");
    pub const RISK_LEVEL: StateKey<String> = StateKey::new("derived:call_risk_level");
    pub const IDENTITY_VERIFIED: StateKey<bool> = StateKey::new("identity_verified");
    pub const WILLINGNESS_TO_PAY: StateKey<f64> = StateKey::new("willingness_to_pay");
}

/// A typed state key — carries the key name AND the expected value type.
pub struct StateKey<T> {
    key: &'static str,
    _marker: PhantomData<T>,
}

impl<T> StateKey<T> {
    pub const fn new(key: &'static str) -> Self {
        Self { key, _marker: PhantomData }
    }

    pub fn as_str(&self) -> &'static str {
        self.key
    }
}

// Typed get — no turbofish needed, type inferred from key
impl State {
    pub fn get_key<T: DeserializeOwned>(&self, key: &StateKey<T>) -> Option<T> {
        self.get::<T>(key.as_str())
    }

    pub fn set_key<T: Serialize>(&self, key: &StateKey<T>, value: &T) {
        self.set(key.as_str(), value);
    }

    // Zero-copy variant
    pub fn with_key<T, F, R>(&self, key: &StateKey<T>, f: F) -> Option<R>
    where
        F: FnOnce(&Value) -> R,
    {
        self.with(key.as_str(), f)
    }
}

// S predicates accept StateKey
impl S {
    pub fn key_is_true(key: &'static StateKey<bool>) -> impl Fn(&State) -> bool + Send + Sync {
        move |s: &State| s.with_key(key, |v| v.as_bool().unwrap_or(false)).unwrap_or(false)
    }
}
```

Usage:

```rust
use crate::keys::*;

// Compile-time: key name is a const, typos are caught by the compiler
.transition("verify", S::key_is_true(&IDENTITY_VERIFIED))

// Runtime: typed read, no turbofish
let emotion: Option<String> = state.get_key(&EMOTIONAL_STATE);
```

**Optional runtime validation** — register expected keys at session start and warn on reads of unknown keys:

```rust
impl State {
    pub fn register_keys(&self, keys: &[&str]) {
        for k in keys {
            self.set_committed("_schema:known_keys", /* append to set */);
        }
    }

    pub fn get_validated<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        #[cfg(debug_assertions)]
        if !self.contains(&format!("_schema:known_keys")) {
            // skip validation if no schema registered
        } else if !self.is_known_key(key) {
            tracing::warn!("Reading unknown state key: {key:?}");
        }
        self.get::<T>(key)
    }
}
```

### Cost

- 4 hours for `StateKey<T>` + typed accessors
- 2 hours for runtime validation (optional, debug_assertions only)
- Non-breaking: existing `&str` API untouched. `StateKey` is opt-in.

### Files

- `crates/rs-adk/src/state.rs` — `StateKey<T>`, `get_key()`, `set_key()`, `with_key()`
- `crates/adk-rs-fluent/src/compose/state.rs` — `S::key_is_true()`, `S::key_eq()`
- Per-cookbook: `keys.rs` module declaring const keys

---

## P1-2: Extractor Error Callback

### The Problem

Extractor failures are swallowed. A `tracing::warn!` fires (if the feature is enabled), but there's no user-facing callback. If your LLM returns garbage JSON 50% of the time, you have no visibility outside of log files.

### The Fix

Add `on_extraction_error` callback:

```rust
// callbacks.rs
pub struct EventCallbacks {
    // ... existing ...
    pub on_extraction_error:
        Option<Arc<dyn Fn(String, LlmError) -> BoxFuture<()> + Send + Sync>>,
}

// live.rs (fluent)
impl Live {
    pub fn on_extraction_error<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String, rs_adk::llm::LlmError) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_extraction_error = Some(Arc::new(move |name, err| {
            Box::pin(f(name, err))
        }));
        self
    }
}

// processor.rs — fire on failure
match ext.extract(&window).await {
    Ok(value) => Some((ext.name().to_string(), value)),
    Err(e) => {
        #[cfg(feature = "tracing-support")]
        tracing::warn!(extractor = ext.name(), "Extraction failed: {e}");

        if let Some(cb) = &callbacks.on_extraction_error {
            cb(ext.name().to_string(), e).await;
        }
        None
    }
}
```

Usage:

```rust
Live::builder()
    .on_extraction_error(|name, err| async move {
        metrics::counter!("extractor_failures", "name" => name).increment(1);
        error!("Extractor {name} failed: {err}");
    })
```

### Cost

- 2 hours. One callback field, one builder method, one invocation site.
- Enables monitoring, alerting, and retry logic at the app layer.

### Files

- `crates/rs-adk/src/live/callbacks.rs` — add field
- `crates/rs-adk/src/live/processor.rs` — fire callback
- `crates/adk-rs-fluent/src/live.rs` — builder method

---

## P1-3: Kill the Channel Clone Ceremony

### The Problem

Every async callback closure needs its own clone of shared handles:

```rust
let tx1 = tx.clone();   // for on_audio
let tx2 = tx.clone();   // for on_text
let tx3 = tx.clone();   // for on_turn_complete
let tx4 = tx.clone();   // for on_extracted
let tx5 = tx.clone();   // for on_interrupted
// ... 15 more times
```

This is pure ceremony. 15 lines of `let tx_n = tx.clone()` before every `Live::builder()` chain.

### The Fix

A `SharedContext` struct that callbacks capture by Arc:

```rust
// adk-rs-fluent/src/live.rs

/// Shared context for callback closures. Clone once, capture everywhere.
pub struct CallbackCtx<T: Send + Sync + 'static> {
    inner: Arc<T>,
}

impl<T: Send + Sync + 'static> CallbackCtx<T> {
    pub fn new(inner: T) -> Self {
        Self { inner: Arc::new(inner) }
    }
}

impl<T: Send + Sync> Clone for CallbackCtx<T> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl<T: Send + Sync> std::ops::Deref for CallbackCtx<T> {
    type Target = T;
    fn deref(&self) -> &T { &self.inner }
}
```

Builder method that injects context into all callbacks:

```rust
impl Live {
    /// Register a shared context that all callback closures can capture.
    ///
    /// Instead of cloning `tx` 15 times, clone `ctx` once per closure.
    /// The ctx is Arc-wrapped internally — Clone is just a refcount bump.
    pub fn with_context<T: Send + Sync + 'static>(self, ctx: CallbackCtx<T>) -> LiveWithContext<T> {
        LiveWithContext { live: self, ctx }
    }
}

pub struct LiveWithContext<T: Send + Sync + 'static> {
    live: Live,
    ctx: CallbackCtx<T>,
}

impl<T: Send + Sync + 'static> LiveWithContext<T> {
    /// Register on_audio with automatic context capture.
    pub fn on_audio(self, f: impl Fn(&T, &Bytes) + Send + Sync + 'static) -> Self {
        let ctx = self.ctx.clone();
        self.live.on_audio(move |data| f(&ctx, data));
        self
    }
    // ... repeat for other callbacks ...
}
```

**Simpler alternative** — just document the pattern and provide a macro:

```rust
/// Macro to clone a value for closure capture.
/// Usage: `let_clone!(tx, state, writer);`
macro_rules! let_clone {
    ($($name:ident),+ $(,)?) => {
        $(let $name = $name.clone();)+
    };
}

// Usage:
let_clone!(tx);
.on_audio(move |data| { tx.send(Audio(data)).ok(); })
```

### Cost

- `CallbackCtx`: 4 hours (full API surface for all callbacks)
- `let_clone!` macro: 30 minutes
- **Recommendation**: Ship the macro first. It's 3 lines and eliminates the visual noise immediately. Build `CallbackCtx` later if demand justifies it.

### Files

- `crates/adk-rs-fluent/src/lib.rs` — `let_clone!` macro
- `cookbooks/ui/src/apps/*.rs` — adopt macro

---

## P1-4: Phase History Cap

### The Problem

`PhaseMachine::history: Vec<PhaseTransition>` grows unbounded. Same issue as TranscriptBuffer, smaller scale. For a session with frequent phase cycling (e.g., negotiate → verify → negotiate → verify), history accumulates.

### The Fix

Same pattern as P0-2. VecDeque with cap:

```rust
pub struct PhaseMachine {
    phases: HashMap<String, Phase>,
    current: String,
    history: VecDeque<PhaseTransition>,  // was Vec
    max_history: usize,                   // new, default 50
    phase_entered_at: Instant,
}

impl PhaseMachine {
    fn record_transition(&mut self, transition: PhaseTransition) {
        if self.history.len() >= self.max_history {
            self.history.pop_front();
        }
        self.history.push_back(transition);
    }
}
```

### Cost

- 1 hour. Mechanical change.

### Files

- `crates/rs-adk/src/live/phase.rs`

---

## P1-5: Tool Declaration Caching

### The Problem

`ToolDispatcher::to_tool_declarations()` iterates all tools and builds `Vec<Tool>` every time it's called. `LlmTextAgent::build_request()` calls it every round. For 10 tools × 10 rounds, that's 100 FunctionDeclaration constructions.

### The Fix

Cache the declarations. Invalidate on register:

```rust
pub struct ToolDispatcher {
    tools: HashMap<String, ToolKind>,
    active: Arc<tokio::sync::Mutex<HashMap<String, ActiveStreamingTool>>>,
    default_timeout: Duration,
    cached_declarations: parking_lot::RwLock<Option<Vec<Tool>>>,  // new
}

impl ToolDispatcher {
    pub fn to_tool_declarations(&self) -> Vec<Tool> {
        // Fast path: cached
        if let Some(cached) = self.cached_declarations.read().as_ref() {
            return cached.clone();
        }

        // Build and cache
        let decls = self.build_declarations();
        *self.cached_declarations.write() = Some(decls.clone());
        decls
    }

    pub fn register(&mut self, tool: impl ToolFunction) {
        let tool = Arc::new(tool);
        self.tools.insert(tool.name().to_string(), ToolKind::Function(tool));
        *self.cached_declarations.write() = None;  // invalidate
    }
}
```

### Cost

- 1 hour. Eliminates O(n_tools) work per LlmTextAgent round.

### Files

- `crates/rs-adk/src/tool.rs`

---

## P2-1: T Module → Live Builder Integration

### The Problem

```rust
// This works in fluent_pipeline examples:
let tools = T::google_search() | T::url_context() | T::code_execution();

// But this doesn't work:
Live::builder().tools(tools)  // ← no such method
```

The T module produces `ToolComposite`. The Live builder accepts `ToolDispatcher` or nothing. The composition algebra dead-ends at the builder boundary.

### The Fix

Add `.tools_from()` or make the existing `.tool()` overloaded:

```rust
impl Live {
    /// Add tools from a ToolComposite (T module composition).
    pub fn with_tools(mut self, composite: ToolComposite) -> Self {
        for tool in composite.into_tool_functions() {
            self.register_tool(tool);
        }
        // Add native tools (google_search, code_execution) to config
        for native in composite.native_tools() {
            self.config = self.config.add_tool(native);
        }
        self
    }
}
```

This requires `ToolComposite` to expose `into_tool_functions()` and `native_tools()` — separate custom ToolFunction instances from built-in API tools (google_search, code_execution are SessionConfig-level, not ToolDispatcher-level).

### Cost

- 3 hours. Needs clean separation between ToolFunction (dispatched by us) and native Tool (dispatched by Gemini).
- Completes the composition algebra from module to builder.

### Files

- `crates/adk-rs-fluent/src/compose/tools.rs` — expose decomposition methods
- `crates/adk-rs-fluent/src/live.rs` — `with_tools()` method

---

## P2-2: M Module Integration or Removal

### The Problem

`M::log() | M::retry(3) | M::timeout(Duration::from_secs(30))` compiles to a `MiddlewareComposite`. There's no way to plug it into the Live builder. It's an orphaned abstraction — defined, tested, unused.

### The Decision

**Option A: Wire it in.** Add `.middleware()` to Live builder. Middleware wraps the control lane — intercepts tool calls, adds logging/timing/retry around them.

```rust
Live::builder()
    .middleware(M::log() | M::retry(3) | M::timeout(Duration::from_secs(30)))
```

**Option B: Kill it.** The Live session already has `before_tool_response`, `on_tool_call`, `on_turn_boundary` — these are callback-based middleware. The M module adds a second abstraction for the same concern. YAGNI.

**Recommendation: Option B** for now. The callback-based approach is sufficient. If a pattern emerges where 3+ cookbooks need the same middleware chain, revisit with Option A. Until then, dead code is worse than no code.

### Cost

- Option A: 8 hours (design middleware injection point in processor, handle ordering)
- Option B: 1 hour (delete the module, remove from compose/mod.rs)

### Files

- Option B: `crates/adk-rs-fluent/src/compose/middleware.rs` — delete or mark `#[doc(hidden)]`

---

## P2-3: Smarter Extractor Scheduling

### The Problem

All extractors run on every TurnComplete, even when the transcript hasn't changed meaningfully. A turn where the user says "uh huh" still triggers a full LLM extraction call.

### The Fix

Add a `should_extract` predicate:

```rust
pub trait TurnExtractor: Send + Sync {
    fn name(&self) -> &str;
    fn window_size(&self) -> usize;

    /// Whether to run extraction on this turn. Default: always.
    fn should_extract(&self, window: &[TranscriptTurn]) -> bool {
        // Default: extract if last turn has meaningful content
        window.last()
            .map(|t| t.user.len() > 10 || !t.tool_calls.is_empty())
            .unwrap_or(false)
    }

    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError>;
}
```

In the processor:

```rust
let extraction_futures: Vec<_> = extractors
    .iter()
    .filter(|ext| ext.should_extract(&window))  // skip trivial turns
    .filter_map(|ext| { /* existing logic */ })
    .collect();
```

Also add extraction result caching — if the transcript window hasn't changed since last extraction, return cached result:

```rust
pub struct CachedExtractor {
    inner: Arc<dyn TurnExtractor>,
    last_window_hash: AtomicU64,
    cached_result: parking_lot::RwLock<Option<Value>>,
}
```

### Cost

- 3 hours for `should_extract()` with default impl
- 4 hours for `CachedExtractor` wrapper
- Saves 100-500ms per trivial turn (skips unnecessary LLM calls)

### Files

- `crates/rs-adk/src/live/extractor.rs` — `should_extract()` default
- `crates/rs-adk/src/live/processor.rs` — filter before `join_all`

---

## P2-4: Connection Pre-Warming at L2

### The Problem

`warm_up()` exists on `BaseLlm` but isn't called automatically. Users have to know it exists and call it manually.

### The Fix

Auto-warm on connect when agent tools are registered:

```rust
// live.rs — in build_and_connect()
if !self.deferred_agent_tools.is_empty() {
    let state = State::new();
    let d = dispatcher.get_or_insert_with(ToolDispatcher::new);
    for deferred in self.deferred_agent_tools {
        d.register(rs_adk::TextAgentTool::from_arc(
            deferred.name, deferred.description, deferred.agent.clone(), state.clone(),
        ));

        // Pre-warm the agent's LLM connection (if it has one)
        // This is fire-and-forget — don't block connect on warm-up
        if let Some(llm_agent) = deferred.agent.as_any().downcast_ref::<LlmTextAgent>() {
            let llm = llm_agent.llm().clone();
            tokio::spawn(async move { let _ = llm.warm_up().await; });
        }
    }
    builder = builder.with_state(state);
}
```

This requires `TextAgent` to expose an `as_any()` method for downcasting, or a simpler approach — add `warm_up()` to the `TextAgent` trait with a default no-op:

```rust
#[async_trait]
pub trait TextAgent: Send + Sync {
    fn name(&self) -> &str;
    async fn run(&self, state: &State) -> Result<String, AgentError>;

    /// Pre-warm any underlying connections. Default: no-op.
    async fn warm_up(&self) -> Result<(), AgentError> { Ok(()) }
}

// LlmTextAgent overrides:
async fn warm_up(&self) -> Result<(), AgentError> {
    self.llm.warm_up().await.map_err(|e| AgentError::Other(e.to_string()))
}
```

Then in `build_and_connect()`:

```rust
for deferred in &self.deferred_agent_tools {
    let agent = deferred.agent.clone();
    tokio::spawn(async move { let _ = agent.warm_up().await; });
}
```

### Cost

- 2 hours. Default no-op on trait + override in LlmTextAgent + spawn in builder.
- First tool call to a TextAgent skips the 100-300ms TLS handshake.

### Files

- `crates/rs-adk/src/text.rs` — `warm_up()` on TextAgent trait + LlmTextAgent override
- `crates/adk-rs-fluent/src/live.rs` — auto-warm in `build_and_connect()`

---

## The Scoreboard

| Gap | Priority | Effort | Impact | What It Fixes |
|-----|----------|--------|--------|---------------|
| Zero-copy state reads | P0 | 4h | High | 12 clones/turn → 0 in guard evaluation |
| Transcript ring cap | P0 | 2h | Medium | Unbounded memory growth |
| Contents clone elimination | P0 | 3h | Medium | N-1 deep clones per text agent run |
| VertexAI token refresh | P0 | 6h | Critical | "Dies after 1 hour" production bug |
| Typed state keys | P1 | 6h | High | Silent typo bugs (entire class eliminated) |
| Extractor error callback | P1 | 2h | Medium | Invisible extraction failures |
| Channel clone ceremony | P1 | 0.5h | Low | 15 lines of boilerplate per cookbook |
| Phase history cap | P1 | 1h | Low | Unbounded memory (smaller scale) |
| Tool declaration caching | P1 | 1h | Low | O(tools) work per text agent round → O(1) |
| T module integration | P2 | 3h | Low | Composition algebra completeness |
| M module decision | P2 | 1h | Low | Dead code removal |
| Extractor scheduling | P2 | 7h | Medium | Skip LLM calls on trivial turns |
| Auto connection warming | P2 | 2h | Low | First-call latency for agent tools |

**Total: ~38.5 hours for everything. P0 alone is 15 hours and fixes the production-critical issues.**

---

## What NOT to Do

1. **Don't add a protobuf codec.** Gemini Live's wire protocol is JSON. A custom codec would need API-side support that doesn't exist. The 33% base64 overhead is the protocol's tax, not ours.

2. **Don't make State lock-free.** DashMap with sharded mutexes is already excellent. A true lock-free map (crossbeam epoch-based) would add complexity for <1ns improvement on an already-uncontended path.

3. **Don't parallelize phase guard evaluation.** Guards are 1-3 closures reading 1-3 state keys. Total: <1μs. Parallelizing would add overhead that exceeds the work.

4. **Don't add an expression compiler for the fluent API.** The 1:1 mapping to L1 is a feature, not a bug. An IR would add a compilation step, error surface, and debugging complexity for zero runtime benefit.

5. **Don't cache extractor results by default.** Extractors read the transcript window, which changes every turn. A cache with proper invalidation is more complex than re-running the extractor. Only add `CachedExtractor` as an opt-in wrapper.

6. **Don't pool TextAgent instances.** They're already Arc'd and stateless (state is passed in). "Pooling" would add a checkout/checkin ceremony for zero benefit.

7. **Don't add streaming to TextAgent.** `BaseLlm::generate()` is request/response by design. If you need streaming, use the Live WebSocket session directly. TextAgent is for fire-and-forget dispatch, not interactive streaming.

8. **Don't add retry to extractors automatically.** An extractor that fails once will likely fail again with the same input. Let the user decide retry policy via `on_extraction_error`. Don't burn 200-500ms on a retry that won't succeed.
