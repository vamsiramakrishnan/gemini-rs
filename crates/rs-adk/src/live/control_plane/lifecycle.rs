//! Turn-complete lifecycle — phases, watchers, temporal, repair, steering.

use std::sync::Arc;

use rs_genai::prelude::SessionEvent;
use rs_genai::session::SessionWriter;

use crate::state::State;

use crate::live::callbacks::EventCallbacks;
use crate::live::computed::ComputedRegistry;
use crate::live::extractor::{ExtractionTrigger, TurnExtractor};
use crate::live::needs::RepairAction;
use crate::live::phase::{PhaseMachine, TransitionResult};
use crate::live::processor::{ControlPlaneConfig, SharedState};
use crate::live::steering::{self, SteeringMode};
use crate::live::temporal::TemporalRegistry;
use crate::live::transcript::TranscriptBuffer;
use crate::live::watcher::WatcherRegistry;

use super::dispatch_callback;
use super::extractors::run_extractors;

/// Handle the TurnComplete pipeline: transcript finalization, extraction,
/// phase evaluation, unified instruction composition, watchers, temporal.
///
/// Unified instruction composition: steps 6/9/10 accumulate into a single
/// `resolved_instruction` that is sent once at the end, eliminating the
/// double-send bug.
///
/// Batched context delivery: all model-role context turns (tool advisory,
/// repair nudge, steering context, phase instruction, on_enter_context) are
/// accumulated into a single `context_buffer` and sent as ONE
/// `send_client_content` call, eliminating the burst of separate WebSocket
/// frames that can confuse the model or clash with concurrent user input.
pub(in crate::live) async fn handle_turn_complete(
    callbacks: &EventCallbacks,
    writer: &Arc<dyn SessionWriter>,
    shared: &SharedState,
    extractors: &[Arc<dyn TurnExtractor>],
    state: &State,
    computed: &Option<ComputedRegistry>,
    phase_machine: &Option<tokio::sync::Mutex<PhaseMachine>>,
    watchers: &Option<WatcherRegistry>,
    temporal: &Option<Arc<TemporalRegistry>>,
    transcript_buffer: &mut TranscriptBuffer,
    extraction_turn_tracker: &mut std::collections::HashMap<String, u32>,
    control_plane: &mut ControlPlaneConfig,
) {
    // 1. Reset turn-scoped state
    state.clear_prefix("turn:");

    // 2. Finalize transcript (prefer server transcriptions when available)
    if let Some(input_text) = state.session().get::<String>("last_input_transcription") {
        transcript_buffer.set_input_transcription(&input_text);
    }
    if let Some(output_text) = state.session().get::<String>("last_output_transcription") {
        transcript_buffer.set_output_transcription(&output_text);
    }
    transcript_buffer.end_turn();

    // 3. Snapshot watched keys BEFORE extractors
    let pre_snapshot = watchers.as_ref().map(|w| {
        state.snapshot_values(
            &w.observed_keys()
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
        )
    });

    // 4. Run extractors matching EveryTurn or Interval triggers
    let current_turn = state.session().get::<u32>("turn_count").unwrap_or(0);
    let turn_extractors: Vec<Arc<dyn TurnExtractor>> = extractors
        .iter()
        .filter(|e| match e.trigger() {
            ExtractionTrigger::EveryTurn => true,
            ExtractionTrigger::Interval(n) => {
                let last = extraction_turn_tracker.get(e.name()).copied().unwrap_or(0);
                current_turn.saturating_sub(last) >= n
            }
            ExtractionTrigger::AfterToolCall
            | ExtractionTrigger::OnPhaseChange
            | ExtractionTrigger::OnGenerationComplete => false,
        })
        .cloned()
        .collect();

    run_extractors(&turn_extractors, transcript_buffer, state, callbacks).await;

    // Update interval trackers for extractors that ran
    for ext in &turn_extractors {
        if matches!(ext.trigger(), ExtractionTrigger::Interval(_)) {
            extraction_turn_tracker.insert(ext.name().to_string(), current_turn);
        }
    }

    // 5. Recompute derived state
    if let Some(ref computed) = computed {
        computed.recompute(state);
    }

    // 6. Build transcript window snapshot for phase evaluation
    let transcript_window = transcript_buffer.snapshot_window(5);

    // Unified instruction composition:
    // Instead of sending instruction at each step (6/9/10), we accumulate
    // into resolved_instruction and send ONCE at the end.
    let mut resolved_instruction: Option<String> = None;
    let mut transition_result: Option<TransitionResult> = None;

    // Batched context buffer: all model-role context turns are accumulated here
    // and sent as a SINGLE send_client_content call, eliminating the burst of
    // separate WebSocket frames that can confuse the model or clash with user input.
    let mut context_buffer: Vec<rs_genai::prelude::Content> = Vec::new();
    // Whether to prompt the model after sending the batched context.
    let mut should_prompt = false;

    // 7. Evaluate phase transitions + compute navigation context
    if let Some(ref pm) = phase_machine {
        let mut machine = pm.lock().await;

        // 7a. Evaluate transitions
        if let Some((target, transition_index)) =
            machine.evaluate(state).map(|(s, i)| (s.to_string(), i))
        {
            let turn = state.session().get::<u32>("turn_count").unwrap_or(0);
            let trigger = crate::live::phase::TransitionTrigger::Guard { transition_index };
            let result = machine
                .transition(&target, state, writer, turn, trigger, &transcript_window)
                .await;
            if let Some(tr) = result {
                resolved_instruction = Some(tr.instruction.clone());
                transition_result = Some(tr);
            }
            state.session().set("phase", machine.current());

            // Store current phase's `needs` for ContextBuilder to read.
            if let Some(phase) = machine.current_phase() {
                if phase.needs.is_empty() {
                    state.remove("session:phase_needs");
                } else {
                    state.set("session:phase_needs", phase.needs.clone());
                }
            }
        }

        // 7b. Always compute and store navigation context
        let nav = machine.describe_navigation(state);
        state.session().set("navigation_context", nav);
    }

    // 7c. Run OnPhaseChange extractors (if a transition fired)
    if transition_result.is_some() {
        let phase_change_extractors: Vec<Arc<dyn TurnExtractor>> = extractors
            .iter()
            .filter(|e| matches!(e.trigger(), ExtractionTrigger::OnPhaseChange))
            .cloned()
            .collect();
        run_extractors(
            &phase_change_extractors,
            transcript_buffer,
            state,
            callbacks,
        )
        .await;
    }

    // 7d. Tool availability advisory (Phase 5)
    // When phase transitions change the tool set, add advisory to context buffer
    if transition_result.is_some() && control_plane.tool_advisory {
        if let Some(ref pm) = phase_machine {
            let machine = pm.lock().await;
            if let Some(tools) = machine.active_tools() {
                let prev_tools: Option<Vec<String>> = state.session().get("active_tools");
                let tools_vec: Vec<String> = tools.iter().map(|s| s.to_string()).collect();
                let changed = prev_tools.as_ref() != Some(&tools_vec);
                if changed {
                    state.session().set("active_tools", tools_vec.clone());
                    let tool_names = tools_vec.join(", ");
                    context_buffer.push(rs_genai::prelude::Content::model(format!(
                        "In this phase, I have access to these tools: {}. \
                         I should only use these tools.",
                        tool_names
                    )));
                }
            }
        }
    }

    // 7e. Conversation repair (Phase 6)
    if let Some(ref mut needs_tracker) = control_plane.needs_fulfillment {
        if let Some(ref pm) = phase_machine {
            let machine = pm.lock().await;
            let phase_name = machine.current().to_string();
            if let Some(phase) = machine.current_phase() {
                if !phase.needs.is_empty() {
                    let needs = phase.needs.clone();
                    drop(machine); // release lock before async work
                    match needs_tracker.evaluate(&phase_name, &needs, state) {
                        RepairAction::Nudge {
                            unfulfilled,
                            attempt,
                        } => {
                            context_buffer.push(rs_genai::prelude::Content::model(format!(
                                "I still need to collect: {}. Let me ask about these.",
                                unfulfilled.join(", ")
                            )));
                            if attempt == 1 {
                                should_prompt = true;
                            }
                        }
                        RepairAction::Escalate { unfulfilled } => {
                            state.set("repair:escalation", true);
                            state.set("repair:unfulfilled", unfulfilled);
                        }
                        RepairAction::None => {}
                    }
                }
            }
        }
    }

    // 7f. Context injection steering (Phase 4)
    if matches!(
        control_plane.steering_mode,
        SteeringMode::ContextInjection | SteeringMode::Hybrid
    ) {
        if let Some(ref pm) = phase_machine {
            let machine = pm.lock().await;
            if let Some(phase) = machine.current_phase() {
                let steering_parts = steering::build_steering_context(state, &phase.modifiers);
                if !steering_parts.is_empty() {
                    context_buffer
                        .push(rs_genai::prelude::Content::model(steering_parts.join("\n")));
                }
            }
        }
    }

    // 8. Fire watchers (compare pre vs post snapshots)
    if let (Some(ref watchers), Some(pre)) = (watchers, pre_snapshot) {
        let post_keys: Vec<&str> = watchers
            .observed_keys()
            .iter()
            .map(|s| s.as_str())
            .collect();
        let diffs = state.diff_values(&pre, &post_keys);
        if !diffs.is_empty() {
            let (blocking, concurrent) = watchers.evaluate(&diffs, state);
            for action in blocking {
                action.await;
            }
            for action in concurrent {
                tokio::spawn(action);
            }
        }
    }

    // 9. Check temporal patterns
    if let Some(ref temporal) = temporal {
        let event = SessionEvent::TurnComplete;
        for action in temporal.check_all(state, Some(&event), writer) {
            tokio::spawn(action);
        }
    }

    // 10. Instruction amendment (additive -- appends to phase instruction)
    // Only applies when there was NO phase transition (transition already includes modifiers)
    if transition_result.is_none() {
        if let Some(ref amendment_fn) = callbacks.instruction_amendment {
            if let Some(amendment_text) = amendment_fn(state) {
                let base = if let Some(ref pm) = phase_machine {
                    let pm_guard = pm.lock().await;
                    pm_guard
                        .current_phase()
                        .map(|p| p.instruction.resolve_with_modifiers(state, &p.modifiers))
                } else {
                    None
                };
                if let Some(base_instruction) = base {
                    resolved_instruction =
                        Some(format!("{}\n\n{}", base_instruction, amendment_text));
                }
            }
        }
    }

    // 11. Instruction template (full replacement -- escape hatch, overrides everything)
    if let Some(ref template) = callbacks.instruction_template {
        if let Some(new_instruction) = template(state) {
            resolved_instruction = Some(new_instruction);
        }
    }

    // 12. Instruction delivery (dedup against last sent)
    //
    // Behavior depends on SteeringMode:
    //   - InstructionUpdate / Hybrid: replace the system instruction via
    //     `update_instruction()`.  Sent immediately (different wire message type).
    //   - ContextInjection: accumulate into context_buffer for batched delivery.
    if let Some(instruction) = resolved_instruction {
        match control_plane.steering_mode {
            SteeringMode::InstructionUpdate | SteeringMode::Hybrid => {
                let should_update = {
                    let last = shared.last_instruction.lock();
                    last.as_deref() != Some(&instruction)
                };
                if should_update {
                    *shared.last_instruction.lock() = Some(instruction.clone());
                    writer.update_instruction(instruction).await.ok();
                }
            }
            SteeringMode::ContextInjection => {
                context_buffer.push(rs_genai::prelude::Content::model(instruction));
            }
        }
    }

    // 13. Add on_enter_context content to batch (if phase transition produced context)
    if let Some(ref tr) = transition_result {
        if let Some(ref contents) = tr.context {
            context_buffer.extend(contents.iter().cloned());
        }
        if tr.prompt_on_enter {
            should_prompt = true;
        }
    }

    // 14. SINGLE batched context send
    //
    // All model-role context turns from steps 7d/7e/7f/12/13 are sent as one
    // atomic WebSocket frame.  This eliminates the burst of separate messages
    // that can confuse the model or clash with concurrent user input.
    if !context_buffer.is_empty() {
        writer.send_client_content(context_buffer, false).await.ok();
    }
    // 14b. Prompt trigger (separate frame — turnComplete:true must be its own message)
    if should_prompt {
        writer.send_client_content(vec![], true).await.ok();
    }

    // 15. Turn boundary hook
    if let Some(cb) = &callbacks.on_turn_boundary {
        cb(state.clone(), writer.clone()).await;
    }

    // 16. User turn-complete callback
    if let Some(cb) = &callbacks.on_turn_complete {
        dispatch_callback!(callbacks.on_turn_complete_mode, cb());
    }

    // 17. Update session turn count
    let tc: u32 = state.session().get("turn_count").unwrap_or(0);
    state.session().set("turn_count", tc + 1);

    // 18. Persist session state (Phase 7 -- fire and forget)
    if let Some(ref persistence) = control_plane.persistence {
        let phase_name = if let Some(ref pm) = phase_machine {
            pm.lock().await.current().to_string()
        } else {
            String::new()
        };
        let snapshot = crate::live::persistence::SessionSnapshot {
            state: state.to_hashmap(),
            phase: phase_name,
            turn_count: tc + 1,
            transcript_summary: transcript_buffer.format_window(5),
            resume_handle: shared.resume_handle.lock().clone(),
            saved_at: {
                // Simple ISO 8601 timestamp without chrono dependency
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                format!("{}s", now.as_secs())
            },
        };
        let p = persistence.clone();
        let sid = control_plane
            .session_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        tokio::spawn(async move {
            if let Err(e) = p.save(&sid, &snapshot).await {
                #[cfg(feature = "tracing-support")]
                tracing::warn!("Session persistence failed: {}", e);
                let _ = e;
            }
        });
    }
}
