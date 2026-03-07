# Extraction Pipeline

## What Is Extraction?

Extraction turns unstructured conversation into structured data. As the user
and model talk, extractors analyze the transcript and produce typed JSON:
customer name, order items, emotional state, account numbers. These values
flow into session `State` where they drive phase transitions, trigger
watchers, and inform instruction composition.

## Why Out-of-Band?

Extraction runs on the control lane, not on the conversation path. An LLM
extraction call takes 1-5 seconds. Voice conversations cannot pause for that.
Extractors run concurrently after each turn completes while the conversation
continues uninterrupted.

```
Turn completes
  -> Transcript buffer finalizes the turn
  -> Extractors run concurrently (control lane)
  -> Results written to State under derived: prefix
  -> Watchers evaluate -> Phase transitions fire
  -> Conversation continues (no blocking)
```

## TurnExtractor

The base trait for all extractors. Implement it for synchronous extraction
(regex, keyword matching, heuristics):

```rust,ignore
use async_trait::async_trait;
use rs_adk::live::extractor::TurnExtractor;
use rs_adk::live::transcript::TranscriptTurn;
use rs_adk::llm::LlmError;

struct OrderNumberExtractor;

#[async_trait]
impl TurnExtractor for OrderNumberExtractor {
    fn name(&self) -> &str { "order_info" }  // State key for results
    fn window_size(&self) -> usize { 5 }     // Look at last 5 turns

    fn should_extract(&self, window: &[TranscriptTurn]) -> bool {
        // Skip trivial turns -- checked before async extraction
        window.last()
            .map(|t| t.user.split_whitespace().count() >= 3)
            .unwrap_or(false)
    }

    async fn extract(&self, window: &[TranscriptTurn]) -> Result<serde_json::Value, LlmError> {
        let text: String = window.iter()
            .map(|t| format!("{} {}", t.user, t.model))
            .collect::<Vec<_>>().join(" ");

        let re = regex::Regex::new(r"order\s+#?(\d+)").unwrap();
        let mut result = serde_json::Map::new();
        if let Some(caps) = re.captures(&text) {
            result.insert("order_number".into(), serde_json::json!(caps[1].to_string()));
        }
        Ok(serde_json::Value::Object(result))
    }
}
```

`should_extract` is checked before launching async work. Return `false` to
skip the LLM round-trip entirely on trivial turns.

## LlmExtractor

For extraction requiring understanding (sentiment, intent, entity
recognition), `LlmExtractor` sends the transcript to an OOB LLM:

```rust,ignore
use rs_adk::live::extractor::LlmExtractor;

let extractor = LlmExtractor::new(
    "SentimentAnalysis",
    llm,  // Arc<dyn BaseLlm>
    "Analyze conversation sentiment and extract the customer's emotional state.",
    3,    // window size
)
.with_schema(serde_json::json!({
    "type": "object",
    "properties": {
        "sentiment": { "type": "string", "enum": ["positive", "neutral", "negative"] },
        "score": { "type": "number" }
    }
}))
.with_min_words(5);  // Skip "uh huh", "ok", "yes" turns
```

## Schema Definition

The fluent API's `extract_turns` auto-generates the schema from a Rust struct:

```rust,ignore
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, JsonSchema)]
struct DebtorState {
    /// "calm", "cooperative", "frustrated", "angry"
    emotional_state: Option<String>,
    /// 0.0 (refusing) to 1.0 (eager)
    willingness_to_pay: Option<f32>,
    /// "full_pay", "partial_pay", "dispute", "refuse", "delay"
    negotiation_intent: Option<String>,
    /// Whether debtor explicitly requested cease-and-desist
    cease_desist_requested: Option<bool>,
}

Live::builder()
    .extract_turns::<DebtorState>(
        llm,
        "Extract: emotional state, willingness to pay, negotiation intent, cease-and-desist.",
    )
    .connect(config).await?;
```

The type name (`DebtorState`) becomes the extractor name and State key.
`extract_turns` auto-enables transcription, generates the JSON schema, and
defaults to a 3-turn window. Use `extract_turns_windowed` for custom sizes.

## Extraction Triggers

By default, extractors run on every `TurnComplete` event. For many use cases
this is wasteful -- trivial utterances ("yeah", "ok") rarely contain extractable
data, and each extraction is an OOB LLM call. Extraction triggers control
*when* extractors fire:

```rust,ignore
use rs_adk::live::extractor::ExtractionTrigger;

Live::builder()
    // Extract every 2 turns instead of every turn (reduces LLM costs by ~50%)
    .extract_turns_triggered::<DebtorState>(
        llm,
        "Extract debtor emotional state and negotiation intent",
        5,  // transcript window size
        ExtractionTrigger::Interval(2),
    )
    .connect(config).await?;
```

| Trigger | When it fires | Use case |
|---------|--------------|----------|
| `EveryTurn` | After every TurnComplete | Default — high-frequency extraction |
| `Interval(n)` | Every N turns | Reduce LLM costs for slow-changing data |
| `AfterToolCall` | After tool dispatch completes | Extract from tool results |
| `OnPhaseChange` | When phase transitions fire | Re-extract on context shift |

The `TurnExtractor` trait also has a `trigger()` method with a default
implementation returning `EveryTurn`, so custom extractors get the old
behavior for free:

```rust,ignore
impl TurnExtractor for MyExtractor {
    fn trigger(&self) -> ExtractionTrigger {
        ExtractionTrigger::AfterToolCall
    }
    // ...
}
```

## Transcript Window

Extractors receive a slice of `TranscriptTurn` values:

```rust,ignore
pub struct TranscriptTurn {
    pub turn_number: u32,
    pub user: String,                    // Accumulated user speech
    pub model: String,                   // Accumulated model speech
    pub tool_calls: Vec<ToolCallSummary>,
    pub timestamp: Instant,
}
```

The `TranscriptBuffer` is a ring buffer (default 50 turns) that evicts the
oldest turns to prevent unbounded memory growth:

```rust,ignore
let mut buf = TranscriptBuffer::new();
let recent = buf.window(3);          // last 3 completed turns
let formatted = buf.format_window(3); // human-readable text
let snapshot = buf.snapshot_window(5); // cheap read-only clone for callbacks
```

## Auto-Flatten

When an extractor returns a JSON object, the framework automatically flattens
it to individual state keys under the `derived:` prefix. Given this result:

```json
{ "emotional_state": "frustrated", "willingness_to_pay": 0.3 }
```

The framework writes:
- `derived:emotional_state` = `"frustrated"`
- `derived:willingness_to_pay` = `0.3`

The prefix is transparent -- `state.get("emotional_state")` auto-checks
`derived:emotional_state` if the unprefixed key is not found:

```rust,ignore
.transition("close", S::is_true("cease_desist_requested"))
// Internally checks derived:cease_desist_requested
```

## Concurrent Extraction

Multiple extractors run in parallel via `futures::future::join_all`:

```rust,ignore
Live::builder()
    .extractor(Arc::new(regex_extractor))      // instant
    .extract_turns::<DebtorState>(llm, "...")   // 1-3 seconds
    .on_extracted(|name, value| async move {
        println!("Extractor '{name}' produced: {value}");
    })
    .on_extraction_error(|name, error| async move {
        eprintln!("Extractor '{name}' failed: {error}");
    })
    .connect(config).await?;
```

## Extraction to State to Watchers

The full data flow after each turn:

1. **Extractors run** concurrently on the control lane.
2. **Results auto-flatten** -- each JSON field becomes a `derived:` key.
3. **Computed state evaluates** -- derived variables that depend on extracted
   keys re-compute.
4. **Watchers fire** -- any watcher observing a changed key triggers.
5. **Phase transitions evaluate** -- guards check, machine transitions.

```rust,ignore
Live::builder()
    .extract_turns::<DebtorState>(llm, "Extract emotional state")
    .computed("call_risk_level", &["derived:sentiment_score"], |state| {
        let score: f64 = state.get("derived:sentiment_score").unwrap_or(0.5);
        if score < 0.3 { Some(json!("high")) } else { Some(json!("low")) }
    })
    .watch("derived:call_risk_level")
        .changed_to(json!("high"))
        .then(|_old, _new, state| async move {
            state.set("alert:risk_escalation", true);
        })
    .phase("negotiate")
        .instruction("Negotiate payment")
        .transition("close", S::is_true("cease_desist_requested"))
        .done()
    .connect(config).await?;
```

## Real Example

The debt collection cookbook combines regex and LLM extractors:

```rust,ignore
// Regex: captures dollar amounts, phone numbers, disclosure acknowledgment
let regex_extractor = Arc::new(RegexExtractor::new("debt_fields", 10, |text, existing| {
    let mut extracted = HashMap::new();
    if !existing.contains_key("dollar_amount") {
        if let Some(m) = DOLLAR_RE.find(text) {
            extracted.insert("dollar_amount".into(), json!(m.as_str()));
        }
    }
    if !existing.contains_key("disclosure_given") {
        if DISCLOSURE_ACK_RE.is_match(text) {
            extracted.insert("disclosure_given".into(), json!(true));
        }
    }
    extracted
}));

let handle = Live::builder()
    .extractor(regex_extractor)
    .extract_turns::<DebtorState>(llm, "Extract debtor emotional state and intent")
    .computed("sentiment_score", &["emotional_state"], |state| {
        let emotion: String = state.get("emotional_state")?;
        Some(json!(match emotion.as_str() {
            "cooperative" => 0.9, "calm" => 0.7,
            "frustrated" => 0.4, "angry" => 0.2, _ => 0.5,
        }))
    })
    .computed("call_risk_level", &["derived:sentiment_score", "cease_desist_requested"], |state| {
        let sentiment: f64 = state.get("derived:sentiment_score").unwrap_or(0.5);
        let cease: bool = state.get("cease_desist_requested").unwrap_or(false);
        Some(json!(if cease { "critical" } else if sentiment < 0.3 { "high" } else { "low" }))
    })
    .phase("disclosure")
        .instruction("Deliver the Mini-Miranda disclosure")
        .transition("verify_identity", S::is_true("disclosure_given"))
        .transition("close", S::is_true("cease_desist_requested"))
        .done()
    .initial_phase("disclosure")
    .connect(config).await?;
```

Extracted fields flow through the full pipeline: extraction produces raw
values, computed state derives higher-level signals, guards evaluate on every
turn, and the phase machine transitions when conditions are met.
