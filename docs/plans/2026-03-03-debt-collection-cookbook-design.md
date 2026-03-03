# Debt Collection Agent Cookbook Design

**Date**: 2026-03-03
**Status**: Approved
**Scope**: New cookbook app exercising 15 previously-unused L2 fluent API features

## Context

The cookbooks/ui currently has 7 apps migrated to L2 `Live::builder()`. An audit of L2 feature coverage identified 17 builder methods that no existing cookbook exercises. This cookbook is designed to fill those gaps with a realistic contact center scenario.

**Scenario**: FDCPA-compliant debt collection voice agent with identity verification, payment negotiation, emotional monitoring, and real-time compliance enforcement.

## L2 Feature Coverage

| Feature | Debt Collection Use |
|---|---|
| `when_sustained()` | Sustained frustration (30s) → inject empathy instruction |
| `when_rate()` | 3+ objections in 60s → supervisor escalation |
| `when_turns()` | 5 turns no progress → suggest callback |
| `crossed_above()` | willingness_to_pay > 0.7 → transition to payment |
| `crossed_below()` | sentiment_score < 0.3 → de-escalation mode |
| `became_true()` | cease_desist_requested → immediate halt + compliance log |
| `became_false()` | identity_verified revoked → block negotiation |
| `changed_to()` | negotiation_intent == "dispute" → validation workflow |
| `.guard()` | Can't enter negotiate without verified + disclosed + not disputed |
| `.on_exit()` | Log compliance metrics on each phase transition |
| `before_tool_response()` | Redact SSN/account numbers before model sees tool results |
| `on_turn_boundary()` | Inject Mini-Miranda reminder if not yet disclosed |
| `extract_turns::<T>()` | LLM extracts emotional_state, willingness_to_pay, negotiation_intent |
| `on_go_away()` | Gracefully wrap up call when server signals disconnect |
| `computed()` | Derive sentiment_score from emotional_state, call_risk_level from multiple inputs |

**Not covered** (irrelevant to voice debt collection): `url_context()`, `media_resolution()`.

## Architecture

Single-agent, 7-phase conversation flow with compliance-gated transitions. Complexity lives in the rules around transitions (guards, compliance, emotional monitoring), not in routing between agents.

### Phase Machine

```
disclosure → verify_identity → inform_debt → negotiate → arrange_payment → confirm → close
```

| # | Phase | Purpose | Exit Condition | Guard |
|---|-------|---------|----------------|-------|
| 1 | `disclosure` | Deliver Mini-Miranda disclosure | `disclosure_given == true` | None (initial) |
| 2 | `verify_identity` | Ask name, DOB, last 4 SSN. Call `verify_identity` tool. | `identity_verified == true` | None |
| 3 | `inform_debt` | State debt amount, creditor, validation rights. Use `lookup_account`. | `debt_acknowledged == true` OR `debt_status == "disputed"` | `guard(identity_verified)` |
| 4 | `negotiate` | Offer payment options. Use `calculate_payment_plan`. | `willingness_to_pay > 0.7` AND `payment_plan_selected` | `guard(identity_verified AND disclosure_given AND debt_status != "disputed")` |
| 5 | `arrange_payment` | Collect payment method, process via `process_payment`. | `payment_confirmed == true` | `guard(payment_plan_selected)` |
| 6 | `confirm` | Summarize agreement, confirm mailing address. | After summary delivered | `guard(payment_confirmed)` |
| 7 | `close` | Thank debtor, remind of next steps. Terminal. | `.terminal()` | None |

**Special transitions:**
- Any phase → `close` if `cease_desist_requested == true` (watcher triggers immediate halt)
- `inform_debt` → `close` if `debt_status == "disputed"` (must send validation notice)
- `negotiate` → `close` if `when_turns(stalled, 5)` fires (suggest callback)

### State Model

#### LLM-Extracted (via `extract_turns::<DebtorState>()`)

Per-turn LLM analysis of the last 3 conversation turns:

```rust
#[derive(Deserialize)]
struct DebtorState {
    emotional_state: Option<String>,      // "calm", "frustrated", "angry", "cooperative"
    willingness_to_pay: Option<f32>,      // 0.0 (refusing) to 1.0 (eager)
    negotiation_intent: Option<String>,   // "full_pay", "partial_pay", "dispute", "refuse", "delay"
    cease_desist_requested: Option<bool>, // explicit request to stop contact
    debt_acknowledged: Option<bool>,      // debtor acknowledges owing the debt
}
```

#### Regex-Extracted (via `RegexExtractor`)

Fast structured extraction:
- `dollar_amount`: `$X,XXX.XX` patterns
- `account_number`: 8-12 digit patterns
- `date_mentioned`: date patterns for promised payment dates
- `phone_number`: phone patterns for callback

#### Computed State

```rust
// Map emotional_state to numeric score
.computed("sentiment_score", &["emotional_state"], |state| {
    match state.get::<String>("emotional_state")?.as_str() {
        "cooperative" => Some(json!(0.9)),
        "calm" => Some(json!(0.7)),
        "frustrated" => Some(json!(0.4)),
        "angry" => Some(json!(0.2)),
        _ => Some(json!(0.5)),
    }
})

// Derived risk assessment
.computed("call_risk_level", &["sentiment_score", "cease_desist_requested"], |state| {
    let sentiment: f64 = state.get("sentiment_score").unwrap_or(0.5);
    let cease: bool = state.get("cease_desist_requested").unwrap_or(false);
    if cease { Some(json!("critical")) }
    else if sentiment < 0.3 { Some(json!("high")) }
    else if sentiment < 0.5 { Some(json!("medium")) }
    else { Some(json!("low")) }
})
```

### Watchers

#### Numeric

```rust
// Ready to pay → nudge toward payment arrangement
.watch("willingness_to_pay")
    .crossed_above(0.7)
    .then(|old, new, state| async move {
        tx.send(StateUpdate { key: "negotiation_signal", value: json!("ready_to_pay") });
    })

// Sentiment drop → de-escalation
.watch("sentiment_score")
    .crossed_below(0.3)
    .blocking()
    .then(|old, new, state| async move {
        tx.send(StateUpdate { key: "risk_alert", value: json!("de-escalation activated") });
    })
```

#### Boolean

```rust
// Cease-and-desist: immediate halt
.watch("cease_desist_requested")
    .became_true()
    .blocking()
    .then(|old, new, state| async move {
        tx.send(Violation { rule: "cease_desist", severity: "critical", ... });
    })

// Identity verification revoked
.watch("identity_verified")
    .became_false()
    .then(|old, new, state| async move {
        tx.send(StateUpdate { key: "verification_revoked", value: json!(true) });
    })
```

#### Value

```rust
// Debt disputed → validation workflow
.watch("negotiation_intent")
    .changed_to(json!("dispute"))
    .blocking()
    .then(|old, new, state| async move {
        tx.send(StateUpdate { key: "debt_status", value: json!("disputed") });
    })
```

### Temporal Patterns

```rust
// Sustained frustration for 30s
.when_sustained("sustained_frustration",
    |state| state.get::<f64>("sentiment_score").map_or(false, |s| s < 0.4),
    Duration::from_secs(30),
    |state, writer| async move {
        tx.send(StateUpdate { key: "temporal_alert", value: json!("sustained_frustration") });
    }
)

// 3+ rapid objections in 60s
.when_rate("rapid_objections",
    |evt| matches!(evt, SessionEvent::TurnComplete),
    3, Duration::from_secs(60),
    |state, writer| async move {
        tx.send(StateUpdate { key: "escalation", value: json!("supervisor_recommended") });
    }
)

// 5 turns stalled
.when_turns("stalled_conversation",
    |state| !state.get::<bool>("progress_made").unwrap_or(false),
    5,
    |state, writer| async move {
        tx.send(StateUpdate { key: "temporal_alert", value: json!("stalled_suggest_callback") });
    }
)
```

### Outbound Interceptors

#### `before_tool_response()` — PII Redaction

All tool responses pass through this interceptor before the model or transcript see them:
- SSN: `123-45-6789` → `***-**-6789`
- Account ID: `78234561` → `****4561`
- Address fields: stripped entirely

```rust
.before_tool_response(|tool_name, response| {
    let mut redacted = response.clone();
    if let Some(obj) = redacted.as_object_mut() {
        if let Some(ssn) = obj.get("ssn").and_then(|v| v.as_str()) {
            obj.insert("ssn".into(), json!(format!("***-**-{}", &ssn[7..])));
        }
        if let Some(acct) = obj.get("account_id").and_then(|v| v.as_str()) {
            let last4 = &acct[acct.len().saturating_sub(4)..];
            obj.insert("account_id".into(), json!(format!("****{last4}")));
        }
        obj.remove("address");
    }
    redacted
})
```

#### `on_turn_boundary()` — Compliance Reminders

```rust
.on_turn_boundary(|state, turn_count| {
    let disclosed: bool = state.get("disclosure_given").unwrap_or(false);
    if !disclosed && turn_count >= 2 {
        return Some("CRITICAL: You MUST deliver the Mini-Miranda disclosure NOW.".into());
    }
    None
})
```

#### `on_go_away()` — Graceful Shutdown

```rust
.on_go_away(|duration| {
    let tx = tx_goaway.clone();
    async move {
        tx.send(StateUpdate { key: "session_ending", value: json!({"reason": "server_goaway"}) });
    }
})
```

### Mock Tools

5 mock functions with realistic data:

| Tool | Input | Output | Redacted? |
|---|---|---|---|
| `lookup_account` | `account_id: String` | `{account_id, debtor_name, ssn, balance, creditor, last_payment_date, days_past_due, payment_history: [...]}` | Yes — SSN masked, account masked, address stripped |
| `verify_identity` | `name, dob, last4ssn` | `{verified: bool, reason: String}` | No |
| `calculate_payment_plan` | `total: f64, months: u32` | `{monthly_payment: f64, interest_rate: f64, total_cost: f64, first_due_date: String}` | No |
| `process_payment` | `account_id, amount, method` | `{confirmation_id: String, status: String, processed_at: String}` | Yes — account masked |
| `log_compliance_event` | `event_type, details` | `{logged: true, event_id: String}` | No |

### Browser Notifications

The devtools panel shows:
- **PhaseChange**: Current phase with compliance status badge
- **StateUpdate**: All extracted state with redacted account info
- **Violation**: Cease-and-desist, compliance breaches
- **Temporal alerts**: Sustained frustration, rapid objections, stalled conversation
- **Risk level**: Computed call_risk_level (low/medium/high/critical)
- **Compliance log**: Audit trail of disclosure, verification, phase transitions

### CookbookApp Metadata

```rust
fn name(&self) -> &str { "debt-collection" }
fn description(&self) -> &str { "FDCPA-compliant debt collection with compliance gates, emotional monitoring, and payment negotiation" }
fn category(&self) -> AppCategory { AppCategory::Showcase }
fn features(&self) -> Vec<String> {
    vec![
        "phase-machine", "compliance-gates", "temporal-patterns",
        "llm-extraction", "tool-response-redaction", "numeric-watchers",
        "computed-state", "turn-boundary-injection",
    ]
}
```

## What Stays Unchanged

- `CookbookApp` trait and `AppRegistry`
- `ServerMessage` / `ClientMessage` enums
- `build_session_config()` helper
- `wait_for_start()`, `send_app_meta()`, `resolve_voice()` helpers
- `RegexExtractor` from `extractors.rs`
- Frontend HTML/JS/CSS
- WebSocket handler (`main.rs` / `app.rs`)

## New Files

- `cookbooks/ui/src/apps/debt_collection.rs` — the cookbook app (~350-400 lines)

## Modified Files

- `cookbooks/ui/src/apps/mod.rs` — add `mod debt_collection;` and register it

## Testing

- Unit tests for mock tool functions (lookup, verify, calculate, process)
- Unit tests for regex extraction (dollar amounts, account numbers, dates)
- Unit tests for PII redaction logic
- Unit tests for computed state derivation (sentiment_score, call_risk_level)
- Existing 67 tests remain unchanged

## Implementation Order

1. Add `mod debt_collection` to mod.rs, register in `register_all()`
2. Create `debt_collection.rs` with CookbookApp impl, constants, instructions
3. Implement mock tools (5 functions + tool dispatcher)
4. Implement regex extractor for structured fields
5. Implement LLM extraction struct (`DebtorState`)
6. Implement PII redaction (`before_tool_response`)
7. Build phase machine (7 phases with guards + on_exit)
8. Add watchers (numeric, boolean, value)
9. Add temporal patterns (sustained, rate, turns)
10. Add outbound interceptors (turn_boundary, on_go_away)
11. Wire up computed state and instruction_template
12. Add browser notification callbacks
13. Unit tests
14. Full build + test verification
