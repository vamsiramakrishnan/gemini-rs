# Turn-Windowed Extraction Pipeline Design

**Goal:** Add first-class handles for transcript accumulation → OOB LLM structured extraction → conversation state tracking, composable via the fluent Live builder API.

**Architecture:** TranscriptBuffer (automatic in processor) + TurnExtractor trait (pluggable OOB LLM) + State integration (concurrent read/write) + fluent builder handles (extract_turns, on_extracted).

## Problem

Today, wiring transcript accumulation + OOB extraction requires ~50 lines of boilerplate per use case: shared Arc/Mutex transcript buffers, manual accumulation in callbacks, manual LLM calls in on_turn_complete, manual deserialization. The primitives exist (State, BaseLlm, TypedTool, transcript events) but the connective tissue is missing.

## Design

### Pipeline

```
Input/Output Transcripts (fast lane, automatic)
    │
    ▼
TranscriptBuffer (accumulate per-turn)
    │
    ▼ on TurnComplete (control lane)
    │
TurnExtractor(s) (OOB LLM + schema → typed Value)
    │
    ├─► State.set(name, value)  ← readable from LiveHandle.state()
    └─► on_extracted callback   ← react (update_instruction, UI, etc.)
```

### L1 Components (rs-adk)

**TranscriptBuffer** (`live/transcript.rs`):
- `TranscriptTurn { turn_number, user, model, timestamp }`
- `TranscriptBuffer { turns, current_user, current_model }`
- Thread-safe via `Arc<parking_lot::Mutex<...>>`
- Fast lane pushes deltas; control lane calls `end_turn()` on TurnComplete
- `window(n)` returns last N completed turns
- `format_window(n)` renders LLM-ready text

**TurnExtractor** (`live/extractor.rs`):
- Trait: `async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError>`
- `LlmExtractor`: BaseLlm-backed impl with prompt + optional JSON schema
- Stores `window_size` so each extractor knows how much context it needs

**Processor integration** (`live/processor.rs`):
- `spawn_event_processor` gains: `transcript_buffer`, `extractors`, `state`
- Fast lane: `push_input()`/`push_output()` on transcript events
- Control lane on TurnComplete: `end_turn()` → run extractors → store in State → fire callback

**EventCallbacks addition**:
- `on_extracted: Option<Arc<dyn Fn(String, Value) -> BoxFuture<()> + Send + Sync>>`
- First arg is extractor name, second is the extracted Value

**LiveHandle addition**:
- `state() -> &State` — read extraction results at any time

### L2 Fluent API (adk-rs-fluent)

```rust
let handle = Live::builder()
    .model(GeminiModel::Gemini2_0FlashLive)
    .instruction("You are a restaurant order assistant")
    // Implicitly enables transcription
    .extract_turns::<OrderState>(
        flash_llm,
        "Extract: items ordered, quantities, modifications, order_phase",
    )
    // Optional: react to extractions
    .on_extracted(|name, value| async move {
        println!("Extracted {name}: {value}");
    })
    .connect_vertex(project, location, token)
    .await?;

// Read latest extraction from shared State at any time:
let order: Option<OrderState> = handle.state().get("OrderState");
```

### Multiple Extractors

```rust
.extract_turns::<OrderState>(order_llm, "Extract order details")
.extract_turns::<Sentiment>(sentiment_llm, "Rate sentiment")
.extract_turns::<Entities>(entity_llm, "Extract named entities")
```

Each runs independently on TurnComplete, stores under its type name.

## Implementation Tasks

1. TranscriptBuffer + tests (~80 LoC)
2. TurnExtractor trait + LlmExtractor + tests (~100 LoC)
3. Wire into processor (~60 LoC changes)
4. Add State to LiveHandle + on_extracted callback (~30 LoC)
5. Fluent handles on Live builder (~50 LoC)
6. Integration test (~40 LoC)

~360 LoC total.
