# Overhead Analysis: gemini-adk-rs & gemini-adk-fluent-rs over gemini-genai-rs (Wire)

**Date**: 2026-03-03
**Scope**: Runtime and structural overhead introduced by the two higher-level crates (`gemini-adk-rs`, `gemini-adk-fluent-rs`) relative to the wire-level crate (`gemini-genai-rs`).

---

## 1. Scale at a Glance

| Metric                        | gemini-genai-rs (wire) | gemini-adk-rs         | gemini-adk-fluent-rs   |
|-------------------------------|-----------------|----------------|-----------------|
| Lines of Rust                 | 11,307          | 23,143         | 4,803           |
| `Arc<` references             | 12              | 231            | 37              |
| `.clone()` call sites         | 55              | 309            | 33              |
| `dyn` trait objects            | 11              | 262            | 36              |
| `#[async_trait]` annotations  | 9               | 100            | 12              |
| `Box::pin` / `Pin<Box<`       | 0               | 16             | 16              |
| `tokio::spawn` call sites     | 4               | 37             | 0               |
| `DashMap` / `Mutex` / `RwLock`| 14              | 81             | 2               |
| Channel references            | 52              | 56             | 0               |
| `HashMap` / `BTreeMap`        | 0               | 61             | 2               |
| Extra crate deps (non-shared) | —               | dashmap, regex, once_cell, uuid | —          |

**Key ratio**: gemini-adk-rs is 2× the code of gemini-genai-rs, introduces 19× more Arc usage, 24× more trait objects, 11× more async_trait boundaries, and 6× more clone sites.

---

## 2. Overhead Categories

### 2.1 Dynamic Dispatch Explosion

**Wire baseline**: gemini-genai-rs uses generics (`connect_with<T: Transport, C: Codec>`) for its hot path. Transport and Codec are monomorphized — zero vtable indirection at runtime.

**gemini-adk-rs**: Nearly every abstraction is trait-object-based:

| Trait object                            | Count | Hot path? |
|-----------------------------------------|-------|-----------|
| `Arc<dyn Agent>`                        | many  | yes       |
| `Arc<dyn ToolFunction>`                 | per tool | yes    |
| `Arc<dyn StreamingTool>`               | per tool | yes    |
| `Arc<dyn Middleware>`                   | per MW | yes      |
| `Arc<dyn Plugin>`                       | per plugin | yes  |
| `Arc<dyn SessionWriter>`               | 1     | yes       |
| `Arc<dyn MemoryService>`               | 1     | no        |
| `Arc<dyn SessionService>`              | 1     | no        |
| `Arc<dyn BaseLlm>`                     | 1     | yes       |
| `Arc<dyn Fn(...) -> Pin<Box<...>>>`    | per callback | yes |

Every callback, every tool invocation, every middleware hook, every agent dispatch goes through vtable indirection. This means:
- **No inlining** across trait boundaries
- **No devirtualization** (Rust/LLVM can devirtualize only in trivial cases)
- **Pointer chase per call** (data ptr + vtable ptr)

**gemini-adk-fluent-rs**: Adds another 36 `dyn` sites, mostly wrapping gemini-adk-rs's types in its own builder closures. These are primarily construction-time, not hot-path.

**Cost**: ~2-5ns per vtable call (L1-cached). On a tool-call round-trip with 4-5 dyn dispatches, this is ~10-25ns of pure dispatch overhead — negligible against network latency but measurable in tight loops.

### 2.2 Async Trait Boxing (`#[async_trait]`)

**Wire baseline**: 9 async_trait annotations, used on trait definitions (Transport, Codec, AuthProvider). The hot path (`generic_run_session`) is a concrete async fn — no boxing.

**gemini-adk-rs**: 100 async_trait annotations. Every `Agent::run()`, `ToolFunction::call()`, `Middleware::on_*()`, `Plugin::on_*()` method allocates a `Pin<Box<dyn Future>>` on every invocation.

This means:
- **1 heap allocation per async call** (the boxed future state machine)
- **1 deallocation when the future completes**
- **No stack-pinning possible** (the future must be heap-allocated for trait object safety)

**gemini-adk-fluent-rs**: 12 more async_trait boundaries, plus 16 explicit `Box::pin` sites for callback closures.

**Cost**: Each boxed future is typically 64-256 bytes (depends on captured state). A tool-call round-trip through gemini-adk-rs allocates 3-6 boxed futures. At ~50ns per alloc/dealloc cycle, this is ~150-300ns per tool call.

### 2.3 Arc Proliferation and Reference Counting

**Wire baseline**: 12 Arc references total. Used for `SessionHandle` internals (state, task handle) — created once per session, cloned rarely.

**gemini-adk-rs**: 231 Arc references. Arcs are the default ownership model:
- Every agent: `Arc<dyn Agent>`
- Every tool: `Arc<dyn ToolFunction>`
- Every sub-agent: `Vec<Arc<dyn Agent>>`
- Every middleware: `Arc<dyn Middleware>`
- Every callback: `Arc<dyn Fn(...)>`
- State container: `Arc<DashMap<...>>`

Arc clone is an atomic increment (~5-15ns, more under contention). Arc drop is an atomic decrement + potential deallocation. On modern x86, atomic ops serialize the memory bus for the cache line.

**Worst case**: Agent transfer in gemini-adk-rs clones the entire agent registry (`HashMap<String, Arc<dyn Agent>>`), incrementing N Arc refcounts plus N String clones.

### 2.4 Clone-Heavy Patterns

**Wire baseline**: 55 clone sites. Most are cheap (Arc bump, Bytes refcount) or one-time (setup config).

**gemini-adk-rs**: 309 clone sites — 5.6× more. Major clone-heavy patterns:

1. **Copy-on-write builders**: `AgentBuilderInner` clones the entire struct (name, instruction, tools Vec, sub_agents Vec, etc.) on every setter call
2. **Broadcast event fan-out**: Every `SessionEvent` is cloned for each subscriber
3. **State snapshots**: `DashMap` contents cloned for state reads
4. **Turn history**: `Vec<Content>` cloned when completing turns
5. **Tool call data**: `FunctionCall` structs cloned for event emission
6. **Agent transfer**: Registry HashMap cloned during transfer

**gemini-adk-fluent-rs**: 33 clone sites, mostly Arc bumps during builder compilation. The copy-on-write `AgentBuilder` pattern means every chained `.instruction()`, `.tool()`, `.sub_agent()` call clones the entire inner struct.

### 2.5 Lock Contention Surface

**Wire baseline**: 14 lock sites. `parking_lot::Mutex` for:
- Resume handle (rare access)
- Current turn tracking (per server message)
- Turn history (per turn completion)

All critical sections are tiny (read/write a field).

**gemini-adk-rs**: 81 lock sites. Adds:
- `DashMap` for state (sharded, but still locked per shard)
- `DashMap` for memory service (nested — two DashMaps deep)
- `tokio::sync::Mutex` for active streaming tools
- `parking_lot::Mutex` for latency tracking middleware
- `parking_lot::Mutex` for audit log middleware

**DashMap overhead**: Each `get()` or `insert()` hashes the key, selects a shard, locks that shard's RwLock, and clones the value out. For JSON `Value` keys (strings), this is hash + lock + String clone + Value clone per access.

**gemini-adk-fluent-rs**: Only 2 lock sites (audit log in middleware compose module). Minimal.

### 2.6 Channel & Task Spawn Overhead

**Wire baseline**:
- 1 `mpsc::channel(256)` — commands to connection loop
- 1 `broadcast::channel(512)` — events to subscribers
- 1 `watch::channel` — phase state
- 4 `tokio::spawn` sites (connection loop, reconnect)

**gemini-adk-rs adds**:
- Additional `broadcast::Sender<InputEvent>` for input fan-out
- Additional `broadcast::Sender<AgentEvent>` for agent lifecycle events
- 37 `tokio::spawn` sites:
  - Per streaming tool: 2 spawns (tool task + collector task)
  - Input streaming tools: additional spawn
  - Background agent tasks
  - Temporal pattern watchers

Each `tokio::spawn` allocates a task (~128-256 bytes), schedules it on the runtime, and requires a waker allocation. For streaming tools, this means 2-3 task spawns per active tool.

**gemini-adk-fluent-rs**: 0 spawns. All its work happens at build time or delegates to gemini-adk-rs.

### 2.7 JSON / Serde Overhead

**Wire baseline**: JSON is the protocol format — serialization/deserialization is unavoidable:
- `serde_json::to_vec()` per outgoing message
- `serde_json::from_str()` per incoming message
- base64 encode/decode for audio data

**gemini-adk-rs amplifies JSON cost**:
- `serde_json::Value` as the universal state currency — every state get/set round-trips through JSON
- Tool parameters arrive as `Value`, dispatched as `Value`, returned as `Value`
- Schema generation (`schemars::schema_for!`) at tool registration
- `HashMap<String, Value>` for event actions / state deltas
- State `DashMap<String, Value>` stores everything as JSON values

The problem: JSON `Value` is a recursive enum (~72 bytes per node) that heap-allocates for every string, array, and object. Passing a simple `{count: 5}` through state involves: serialize to Value → DashMap insert (clone) → DashMap get (clone) → deserialize from Value. That's 3 allocations + 2 clones for a single integer.

**gemini-adk-fluent-rs**: Adds JSON schema generation for typed tools and JSON Map creation for state transforms. Relatively minor.

### 2.8 String Allocation Patterns

**Wire baseline**: Strings are used for text content, transcription, and session IDs. Most are created once (config) or are protocol-inherent (text deltas).

**gemini-adk-rs adds pervasive string allocation**:
- Agent names: `String` (cloned on every registry lookup)
- Tool names: `String` (cloned for HashMap keys)
- State keys: `String` (cloned for DashMap access)
- Error messages: `String` (via thiserror)
- Session IDs: `String` (UUID → String)
- Event IDs: `String` (UUID → String on every event)
- Prefixed state keys: `format!("{}:{}", prefix, key)` on every access

UUID generation (`Uuid::new_v4()`) calls the OS random number generator and formats to a 36-byte string. This happens on every event creation, every function call ID, every session creation.

### 2.9 Memory Layout Overhead

**Wire types** are compact:
```
Content:  ~32 bytes (Option<Role> + Vec<Part>)
Part:     ~80 bytes (largest variant: FunctionCall)
Role:     1 byte enum
```

**gemini-adk-rs wraps these in layers**:
```
InvocationContext:
  ├── AgentSession           (~104 bytes)
  │   ├── Arc<dyn SessionWriter>  (16 bytes: ptr + vtable)
  │   ├── broadcast::Sender       (8 bytes)
  │   ├── broadcast::Sender       (8 bytes)
  │   └── State                   (32 bytes: 2× Arc<DashMap>)
  ├── broadcast::Sender<AgentEvent>  (8 bytes)
  ├── MiddlewareChain        (~32 bytes: Vec<Arc<dyn Middleware>>)
  ├── RunConfig              (~64 bytes)
  └── Option<String>         (32 bytes: session_id)
```

Every tool call receives a `ToolContext` that references this entire structure. Every middleware hook receives it. Every agent `run()` receives it.

**gemini-adk-fluent-rs** adds builder structs:
```
AgentBuilderInner:
  ├── name: Option<String>
  ├── instruction: Option<String>
  ├── tools: Vec<Arc<dyn ToolEntryTrait>>
  ├── sub_agents: Vec<Arc<dyn TextAgent>>
  ├── stop_sequences: Vec<String>
  ├── writes: Vec<String>
  ├── reads: Vec<String>
  ├── transfer_to_agent: Option<String>
  ├── llm: Option<Arc<dyn BaseLlm>>
  └── ... (more Optional fields)
```

This is ~200+ bytes per builder, cloned on every chained method call.

---

## 3. Hot-Path Trace: Per-Message Overhead

### 3.1 Incoming Text Delta

| Step | Layer | Allocations | Clones | Locks | Chan sends | dyn calls | Spawns |
|------|-------|-------------|--------|-------|------------|-----------|--------|
| WS recv → decode | wire | 1 (JSON parse) | 0 | 0 | 0 | 0 | 0 |
| Route + emit event | wire | 0 | 1 (text) | 1 (turn) | 1 (broadcast) | 0 | 0 |
| Event router | gemini-adk-rs | 0 | 0 | 0 | 1 (fast lane) | 0 | 0 |
| Fast lane callback | gemini-adk-rs | 0 | 0 | 0 | 0 | 1 (on_text) | 0 |
| **Total** | | **1** | **1** | **1** | **2** | **1** | **0** |

### 3.2 Outgoing Audio Chunk

| Step | Layer | Allocations | Clones | Locks | Chan sends | dyn calls | Spawns |
|------|-------|-------------|--------|-------|------------|-----------|--------|
| send_audio() | gemini-adk-rs | 0 | 0-1 (input fan-out) | 0 | 0-1 (broadcast) | 1 (SessionWriter) | 0 |
| Command enqueue | wire | 1 (command) | 0 | 0 | 1 (mpsc) | 0 | 0 |
| base64 + JSON encode | wire | 2 (b64 + json) | 0 | 0 | 0 | 0 | 0 |
| WS send | wire | 0 | 0 | 0 | 0 | 0 | 0 |
| **Total** | | **3-4** | **0-1** | **0** | **1-2** | **1** | **0** |

### 3.3 Tool Call Round-Trip (single tool)

| Step | Layer | Allocations | Clones | Locks | Chan sends | dyn calls | Spawns |
|------|-------|-------------|--------|-------|------------|-----------|--------|
| Decode tool call | wire | N (parse) | 1 (calls) | 1 (turn) | 1 (broadcast) | 0 | 0 |
| Route to control lane | gemini-adk-rs | 0 | 0 | 0 | 1 | 0 | 0 |
| Dispatch lookup | gemini-adk-rs | 0 | 1 (Arc tool) | 0 | 0 | 0 | 0 |
| Tool function call | gemini-adk-rs | 1 (boxed future) | 0 | 0 | 0 | 1 (tool.call) | 0 |
| Build response | gemini-adk-rs | 1 (FunctionResponse) | 0 | 0 | 0 | 0 | 0 |
| Middleware hooks | gemini-adk-rs | M (boxed futures) | 0 | 0 | 0 | M | 0 |
| Event emission | gemini-adk-rs | 1 (UUID) | 0 | 0 | 1 (broadcast) | 0 | 0 |
| send_tool_response | wire | 2 (JSON) | 0 | 0 | 1 (mpsc) | 0 | 0 |
| **Total** | | **5+N+M** | **2** | **1** | **4** | **1+M** | **0** |

Where N = number of function calls in batch, M = number of middleware.

---

## 4. Structural Overhead (Build / Compile Time)

### 4.1 Dependency Graph

gemini-genai-rs pulls in:
- tokio, tokio-tungstenite, futures-util (async runtime)
- serde, serde_json, base64 (serialization)
- bytes, uuid, thiserror, async-trait

gemini-adk-rs adds:
- **dashmap** (6.x) — sharded concurrent map, brings crossbeam-utils
- **regex** — full regex engine (~1MB compiled), only used for pattern matching in extractors
- **once_cell** — lazy initialization (now in std, arguably unnecessary)
- **schemars** — JSON Schema generation, brings dyn-clone and serde_json schema types

gemini-adk-fluent-rs adds no new dependencies beyond gemini-adk-rs + gemini-genai-rs.

### 4.2 Monomorphization Cost

gemini-genai-rs uses generics for Transport and Codec — this generates specialized code per combination but avoids runtime dispatch. Total monomorphization sites: ~4.

gemini-adk-rs avoids generics almost entirely — everything is `dyn`. This reduces binary size from monomorphization but increases it from vtable generation and `#[async_trait]` desugaring (100 sites × generated wrapper code).

---

## 5. Qualitative Overhead Summary

### What gemini-adk-rs buys (justifying some overhead)

1. **Multi-agent orchestration** — agent transfer, sub-agent trees, parallel/sequential composition
2. **Tool dispatch** — automatic JSON-to-function routing with schema validation
3. **Middleware pipeline** — cross-cutting concerns (logging, retry, latency)
4. **State management** — shared state across agents with delta tracking
5. **Event system** — lifecycle observability (agent start/stop, tool calls, errors)
6. **Streaming tools** — progressive results via channels

### What gemini-adk-rs over-pays for

1. **JSON as universal currency** — state get/set round-trips through serde even for primitive values
2. **Arc<dyn Everything>** — every abstraction boundary is a heap-allocated trait object; no option for static dispatch
3. **Mandatory event emission** — UUID generation + broadcast clone on every lifecycle event, even when no subscribers exist
4. **DashMap for simple state** — sharded concurrent map for what is often single-threaded access within one agent
5. **Copy-on-write builders** — full struct clone on every setter (could use &mut self instead)
6. **Regex dependency** — full regex crate for simple pattern matching that could use string contains/starts_with

### What gemini-adk-fluent-rs buys

1. **Builder ergonomics** — chainable API for agent/tool/live-session construction
2. **Operator algebra** — composable pipeline/fan-out/loop abstractions
3. **Composition modules** — state transforms, context filters, prompt composers, middleware

### What gemini-adk-fluent-rs over-pays for

1. **Copy-on-write AgentBuilder** — Arc<Inner> cloned on every method call; could use &mut self
2. **Callback boxing** — 18+ callback types each wrapped in Arc<Box<dyn Fn>> + Box::pin per invocation
3. **Compilation step** — Composable AST → agent tree involves recursive allocation and Arc propagation

---

## 6. Overhead Relative to Network Latency

For context on whether these overheads matter in practice:

| Operation | Typical latency |
|-----------|----------------|
| Gemini API round-trip (us-central1) | 100-500ms |
| WebSocket frame send/recv | 0.5-2ms |
| JSON serialize 1KB message | 1-5μs |
| base64 encode 16KB audio chunk | 2-8μs |
| Arc clone (uncontended) | 5-15ns |
| Vtable dispatch | 2-5ns |
| Box::pin future alloc | 30-80ns |
| DashMap get (uncontended) | 20-50ns |
| UUID::new_v4() | 50-100ns |
| tokio::spawn | 200-500ns |

**Bottom line**: The per-message overhead from gemini-adk-rs is ~500ns-2μs, which is 1000× smaller than network latency. For a voice conversation doing 30 audio chunks/second, this is ~15-60μs/second — imperceptible.

**Where it could matter**:
- High-frequency tool call loops (100+ calls/second)
- Memory-constrained environments (each agent tree holds many Arcs, DashMaps, channels)
- Compilation time (100 async_trait expansions + dashmap + regex + schemars)

---

## 7. Recommendations for Future Work

These are not action items — they document optimization opportunities if overhead becomes a concern:

1. **State store**: Replace `DashMap<String, Value>` with typed state (generic `T: Serialize`) to avoid JSON round-tripping
2. **Static dispatch option**: Offer generic agent/tool traits alongside dyn versions for hot-path-sensitive use cases
3. **Builder pattern**: Use `&mut self` builders instead of copy-on-write Arc<Inner>
4. **Lazy events**: Only generate UUIDs and broadcast events when subscribers exist
5. **Inline tools**: Allow `FnOnce` tools that don't need Arc wrapping for single-use tools
6. **Remove regex dep**: Replace with simple string matching where full regex isn't needed
7. **Pool boxed futures**: Reuse future allocations for frequently-called async_trait methods
