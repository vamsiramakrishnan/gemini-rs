//! OOB extraction pipeline — run extractors concurrently and merge results into state.

use std::sync::Arc;

use crate::state::State;

use tokio::sync::broadcast;

use crate::live::callbacks::EventCallbacks;
use crate::live::events::LiveEvent;
use crate::live::extractor::{MergePolicy, TurnExtractor};
use crate::live::transcript::TranscriptBuffer;
use serde_json::{json, Value};

use super::dispatch_callback;

/// Run a subset of extractors concurrently and merge results into state.
///
/// Shared between handle_turn_complete (EveryTurn/Interval),
/// handle_tool_calls (AfterToolCall), and phase transitions (OnPhaseChange).
pub(in crate::live) async fn run_extractors(
    extractors: &[Arc<dyn TurnExtractor>],
    transcript_buffer: &mut TranscriptBuffer,
    state: &State,
    callbacks: &EventCallbacks,
    event_tx: &broadcast::Sender<LiveEvent>,
) {
    if extractors.is_empty() {
        return;
    }

    let extraction_futures: Vec<_> = extractors
        .iter()
        .filter_map(|extractor| {
            let window_size = extractor.window_size();
            let window: Vec<_> = transcript_buffer.window(window_size).to_vec();
            if window.is_empty() {
                return None;
            }
            // Check should_extract before launching async work
            if !extractor.should_extract(&window) {
                return None;
            }
            let ext = extractor.clone();
            Some(async move {
                match ext.extract(&window).await {
                    Ok(value) => Ok((ext, value)),
                    Err(e) => {
                        #[cfg(feature = "tracing-support")]
                        tracing::warn!(extractor = ext.name(), "Extraction failed: {e}");
                        Err((ext.name().to_string(), e.to_string()))
                    }
                }
            })
        })
        .collect();

    let results = futures::future::join_all(extraction_futures).await;
    for result in results {
        match result {
            Ok((extractor, value)) => {
                let name = extractor.name().to_string();
                state.set(&name, &value);
                // Emit top-level extraction event
                let _ = event_tx.send(LiveEvent::Extraction {
                    name: name.clone(),
                    value: value.clone(),
                });
                promote_extraction_fields(extractor.as_ref(), &name, &value, state, event_tx);
                if let Some(cb) = &callbacks.on_extracted {
                    dispatch_callback!(callbacks.on_extracted_mode, cb(name, value));
                }
            }
            Err((name, error)) => {
                let _ = event_tx.send(LiveEvent::ExtractionError {
                    name: name.clone(),
                    error: error.clone(),
                });
                if let Some(cb) = &callbacks.on_extraction_error {
                    dispatch_callback!(callbacks.on_extraction_error_mode, cb(name, error));
                }
            }
        }
    }
}

fn promote_extraction_fields(
    extractor: &dyn TurnExtractor,
    name: &str,
    value: &Value,
    state: &State,
    event_tx: &broadcast::Sender<LiveEvent>,
) {
    let Some(obj) = value.as_object() else {
        return;
    };

    let rules = extractor.promotion_rules();
    if rules.is_empty() {
        // Legacy auto-flatten: top-level non-null fields promote to same state key.
        // Null values do not erase previously extracted state.
        for (field, val) in obj {
            if val.is_null() {
                continue;
            }
            state.set(field, val.clone());
            let _ = event_tx.send(LiveEvent::Extraction {
                name: format!("{name}.{field}"),
                value: val.clone(),
            });
        }
        return;
    }

    for rule in rules {
        let Some(val) = obj.get(&rule.field) else {
            continue;
        };
        if val.is_null() {
            emit_promotion_decision(
                event_tx,
                name,
                &rule.field,
                &rule.state_key,
                false,
                "extracted value was null",
                val.clone(),
            );
            continue;
        }
        if rule
            .accept
            .as_ref()
            .is_some_and(|accept| !accept(state, val))
        {
            emit_promotion_decision(
                event_tx,
                name,
                &rule.field,
                &rule.state_key,
                false,
                "promotion predicate rejected the value",
                val.clone(),
            );
            continue;
        }
        if matches!(rule.merge, MergePolicy::KeepKnown) && state.contains(&rule.state_key) {
            emit_promotion_decision(
                event_tx,
                name,
                &rule.field,
                &rule.state_key,
                false,
                "existing state value was kept",
                val.clone(),
            );
            continue;
        }

        state.set(&rule.state_key, val.clone());
        state.set(
            format!("state_meta:{}", rule.state_key),
            json!({
                "source": "extraction",
                "extractor": name,
                "field": rule.field,
            }),
        );
        let _ = event_tx.send(LiveEvent::Extraction {
            name: format!("{name}.{}", rule.field),
            value: val.clone(),
        });
        emit_promotion_decision(
            event_tx,
            name,
            &rule.field,
            &rule.state_key,
            true,
            "promotion rule accepted the value",
            val.clone(),
        );
    }
}

fn emit_promotion_decision(
    event_tx: &broadcast::Sender<LiveEvent>,
    extractor: &str,
    field: &str,
    state_key: &str,
    accepted: bool,
    reason: &str,
    value: Value,
) {
    let _ = event_tx.send(LiveEvent::StatePromotion {
        extractor: extractor.to_string(),
        field: field.to_string(),
        state_key: state_key.to_string(),
        accepted,
        reason: reason.to_string(),
        value,
    });
}

/// Run extractors using a window that optionally includes the current in-progress turn.
///
/// When `include_current` is true, uses `snapshot_window_with_current` to capture
/// the model's output before interruption truncation (for GenerationComplete extractors).
pub(in crate::live) async fn run_extractors_with_window(
    extractors: &[Arc<dyn TurnExtractor>],
    transcript_buffer: &mut TranscriptBuffer,
    state: &State,
    callbacks: &EventCallbacks,
    include_current: bool,
    event_tx: &broadcast::Sender<LiveEvent>,
) {
    if extractors.is_empty() {
        return;
    }

    let extraction_futures: Vec<_> = extractors
        .iter()
        .filter_map(|extractor| {
            let window_size = extractor.window_size();
            let window = if include_current {
                transcript_buffer
                    .snapshot_window_with_current(window_size)
                    .turns()
                    .to_vec()
            } else {
                transcript_buffer.window(window_size).to_vec()
            };
            if window.is_empty() || !extractor.should_extract(&window) {
                return None;
            }
            let ext = extractor.clone();
            Some(async move {
                match ext.extract(&window).await {
                    Ok(value) => Ok((ext, value)),
                    Err(e) => {
                        #[cfg(feature = "tracing-support")]
                        tracing::warn!(extractor = ext.name(), "Extraction failed: {e}");
                        Err((ext.name().to_string(), e.to_string()))
                    }
                }
            })
        })
        .collect();

    let results = futures::future::join_all(extraction_futures).await;
    for result in results {
        match result {
            Ok((extractor, value)) => {
                let name = extractor.name().to_string();
                state.set(&name, &value);
                let _ = event_tx.send(LiveEvent::Extraction {
                    name: name.clone(),
                    value: value.clone(),
                });
                promote_extraction_fields(extractor.as_ref(), &name, &value, state, event_tx);
                if let Some(cb) = &callbacks.on_extracted {
                    dispatch_callback!(callbacks.on_extracted_mode, cb(name, value));
                }
            }
            Err((name, error)) => {
                let _ = event_tx.send(LiveEvent::ExtractionError {
                    name: name.clone(),
                    error: error.clone(),
                });
                if let Some(cb) = &callbacks.on_extraction_error {
                    dispatch_callback!(callbacks.on_extraction_error_mode, cb(name, error));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::extractor::{ExtractionTrigger, FieldPromotion};
    use crate::live::transcript::TranscriptTurn;
    use crate::llm::LlmError;
    use async_trait::async_trait;

    struct MockExtractor {
        name: &'static str,
        value: Value,
        promotions: Vec<FieldPromotion>,
    }

    #[async_trait]
    impl TurnExtractor for MockExtractor {
        fn name(&self) -> &str {
            self.name
        }

        fn window_size(&self) -> usize {
            1
        }

        fn trigger(&self) -> ExtractionTrigger {
            ExtractionTrigger::EveryTurn
        }

        fn promotion_rules(&self) -> &[FieldPromotion] {
            &self.promotions
        }

        async fn extract(&self, _window: &[TranscriptTurn]) -> Result<Value, LlmError> {
            Ok(self.value.clone())
        }
    }

    fn buffer_with_turn() -> TranscriptBuffer {
        let mut buffer = TranscriptBuffer::new();
        buffer.push_input("hello there");
        buffer.push_output("hi");
        buffer.end_turn();
        buffer
    }

    #[tokio::test]
    async fn explicit_promotions_do_not_auto_flatten_unruled_fields() {
        let state = State::new();
        let callbacks = EventCallbacks::default();
        let (event_tx, _event_rx) = broadcast::channel(16);
        let mut buffer = buffer_with_turn();
        let extractor: Arc<dyn TurnExtractor> = Arc::new(MockExtractor {
            name: "DebtorState",
            value: json!({
                "emotional_state": "calm",
                "debt_acknowledged": true,
            }),
            promotions: vec![FieldPromotion::overwrite("emotional_state")],
        });

        run_extractors(&[extractor], &mut buffer, &state, &callbacks, &event_tx).await;

        assert_eq!(
            state.get::<String>("emotional_state").as_deref(),
            Some("calm")
        );
        assert_eq!(
            state.get::<Value>("DebtorState"),
            Some(json!({
                "emotional_state": "calm",
                "debt_acknowledged": true,
            }))
        );
        assert!(!state.contains("debt_acknowledged"));
        assert_eq!(
            state.get::<Value>("state_meta:emotional_state"),
            Some(json!({
                "source": "extraction",
                "extractor": "DebtorState",
                "field": "emotional_state",
            }))
        );
    }

    #[tokio::test]
    async fn promotion_predicate_blocks_until_required_context_exists() {
        let state = State::new();
        let callbacks = EventCallbacks::default();
        let (event_tx, _event_rx) = broadcast::channel(16);
        let extractor: Arc<dyn TurnExtractor> = Arc::new(MockExtractor {
            name: "DebtorState",
            value: json!({ "debt_acknowledged": true }),
            promotions: vec![FieldPromotion::true_only("debt_acknowledged")
                .after_presented("debt_details")],
        });

        let mut first_buffer = buffer_with_turn();
        run_extractors(
            &[extractor.clone()],
            &mut first_buffer,
            &state,
            &callbacks,
            &event_tx,
        )
        .await;
        assert!(!state.contains("debt_acknowledged"));

        state.set("presented:debt_details", true);
        let mut second_buffer = buffer_with_turn();
        run_extractors(
            &[extractor],
            &mut second_buffer,
            &state,
            &callbacks,
            &event_tx,
        )
        .await;
        assert_eq!(state.get::<bool>("debt_acknowledged"), Some(true));
    }

    #[tokio::test]
    async fn promotion_events_explain_accepted_and_blocked_decisions() {
        let state = State::new();
        let callbacks = EventCallbacks::default();
        let (event_tx, mut event_rx) = broadcast::channel(16);
        let mut buffer = buffer_with_turn();
        let extractor: Arc<dyn TurnExtractor> = Arc::new(MockExtractor {
            name: "DebtorState",
            value: json!({
                "cease_desist_requested": false,
                "emotional_state": "frustrated",
            }),
            promotions: vec![
                FieldPromotion::true_only("cease_desist_requested"),
                FieldPromotion::overwrite("emotional_state"),
            ],
        });

        run_extractors(&[extractor], &mut buffer, &state, &callbacks, &event_tx).await;

        let mut blocked = false;
        let mut accepted = false;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                LiveEvent::StatePromotion {
                    state_key,
                    accepted: false,
                    reason,
                    ..
                } if state_key == "cease_desist_requested" => {
                    blocked = reason.contains("predicate");
                }
                LiveEvent::StatePromotion {
                    state_key,
                    accepted: true,
                    ..
                } if state_key == "emotional_state" => {
                    accepted = true;
                }
                _ => {}
            }
        }

        assert!(blocked);
        assert!(accepted);
    }
}
