//! Turn-complete lifecycle — phases, watchers, temporal, repair, steering.

use std::sync::Arc;

use gemini_genai_rs::prelude::SessionEvent;
use gemini_genai_rs::session::SessionWriter;

use crate::state::State;

use crate::live::callbacks::EventCallbacks;
use crate::live::computed::ComputedRegistry;
use crate::live::events::LiveEvent;
use crate::live::extractor::{ExtractionTrigger, TurnExtractor};
use crate::live::needs::RepairAction;
use crate::live::phase::{PhaseMachine, TransitionEvaluation, TransitionResult};
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
    event_tx: &tokio::sync::broadcast::Sender<LiveEvent>,
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

    // 3. Capture a journal cursor before extractor/computed/phase mutations.
    let pre_watcher_cursor = watchers.as_ref().map(|_| state.mutation_cursor());

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

    run_extractors(
        &turn_extractors,
        transcript_buffer,
        state,
        callbacks,
        event_tx,
    )
    .await;

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
    let mut transition_from: Option<String> = None;
    let mut transition_to: Option<String> = None;

    // Batched context buffer: all model-role context turns are accumulated here
    // and sent as a SINGLE send_client_content call, eliminating the burst of
    // separate WebSocket frames that can confuse the model or clash with user input.
    let mut context_buffer: Vec<gemini_genai_rs::prelude::Content> = Vec::new();
    // Whether to prompt the model after sending the batched context.
    let mut should_prompt = false;

    // 7. Evaluate phase transitions + compute navigation context
    if let Some(ref pm) = phase_machine {
        let mut machine = pm.lock().await;

        // 7a. Evaluate transitions and run target preparations when a guarded
        // transition is blocked only by missing required state.
        let mut evaluation = machine.evaluate_for_transition(state);
        if let Some(TransitionEvaluation::Blocked { target, .. }) = &evaluation {
            if machine.prepare_target(target, state, writer).await {
                evaluation = machine.evaluate_for_transition(state);
            }
        }

        if let Some(TransitionEvaluation::Ready {
            target,
            transition_index,
        }) = evaluation
        {
            let from_phase = machine.current().to_string();
            let turn = state.session().get::<u32>("turn_count").unwrap_or(0);
            let trigger = crate::live::phase::TransitionTrigger::Guard { transition_index };
            let result = machine
                .transition(&target, state, writer, turn, trigger, &transcript_window)
                .await;
            if let Some(tr) = result {
                resolved_instruction = Some(tr.instruction.clone());
                transition_from = Some(from_phase);
                transition_to = Some(target.clone());
                transition_result = Some(tr);
            }
            let _ = state.session().set("phase", machine.current());

            // Store current phase's `needs` for ContextBuilder to read.
            if let Some(phase) = machine.current_phase() {
                if phase.needs.is_empty() {
                    state.remove("session:phase_needs");
                } else {
                    let _ = state.set("session:phase_needs", phase.needs.clone());
                }
                if phase.requires.is_empty() {
                    state.remove("session:phase_requires");
                } else {
                    let _ = state.set("session:phase_requires", phase.requires.clone());
                }
            }
        }

        // 7b. Always compute and store navigation context
        let nav = machine.describe_navigation(state);
        let _ = state.session().set("navigation_context", nav);
    }

    // 7c. Emit PhaseTransition LiveEvent (if a transition fired)
    if let (Some(ref from), Some(ref to)) = (&transition_from, &transition_to) {
        let _ = event_tx.send(LiveEvent::PhaseTransition {
            from: from.clone(),
            to: to.clone(),
            reason: format!(
                "guard at turn {}",
                state.session().get::<u32>("turn_count").unwrap_or(0)
            ),
        });
    }

    // 7d. Run OnPhaseChange extractors (if a transition fired)
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
            event_tx,
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
                    let _ = state.session().set("active_tools", tools_vec.clone());
                    let tool_names = tools_vec.join(", ");
                    context_buffer.push(gemini_genai_rs::prelude::Content::model(format!(
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
                            context_buffer.push(gemini_genai_rs::prelude::Content::model(format!(
                                "I still need to collect: {}. Let me ask about these.",
                                unfulfilled.join(", ")
                            )));
                            if attempt == 1 {
                                should_prompt = true;
                            }
                        }
                        RepairAction::Escalate { unfulfilled } => {
                            let _ = state.set("repair:escalation", true);
                            let _ = state.set("repair:unfulfilled", unfulfilled);
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
                    context_buffer.push(gemini_genai_rs::prelude::Content::model(
                        steering_parts.join("\n"),
                    ));
                }
            }
        }
    }

    // 7g. Flow governance: re-latch the marking, project active-step postures
    // as steering, surface unmet requirements as repair, and publish status.
    if let Some(ref mut mon) = control_plane.flow {
        mon.on_turn(state);
        let done: Vec<String> = mon.marking().done.iter().cloned().collect();
        let _ = state.set("flow:done", done);
        let active: Vec<String> = mon
            .active_steps(state)
            .iter()
            .map(|s| s.id.clone())
            .collect();
        let _ = state.set("flow:active", active);
        for posture in mon.active_postures(state) {
            context_buffer.push(gemini_genai_rs::prelude::Content::model(posture));
        }
        // Grounding lines: curated, State-interpolated facts (anti-hallucination).
        for ground in mon.active_grounds(state) {
            context_buffer.push(gemini_genai_rs::prelude::Content::model(ground));
        }
        let unmet = mon.unmet_requirements();
        if !unmet.is_empty() {
            context_buffer.push(gemini_genai_rs::prelude::Content::model(format!(
                "Before finishing, these still need to happen: {}.",
                unmet.join(", ")
            )));
        }
        // Fire on_enter actions for steps that just became active. `Call`
        // actions resolve inline; `Dispatch`/`Background` run detached.
        mon.fire_enter_actions(state).await;
    }

    // 8. Fire watchers from net state mutations since the cursor.
    if let (Some(ref watchers), Some(cursor)) = (watchers, pre_watcher_cursor) {
        let mutations = state.mutations_since(cursor);
        if !mutations.is_empty() {
            let (blocking, concurrent) = watchers.evaluate_mutations(&mutations, state);
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

    // 12–14. Compose the final instruction and deliver it plus any context turns
    // as one batched, dedup'd, delivery-mode-aware step. (Extracted so this
    // scar-heavy block is a named, harness-covered unit — see `harness` below and
    // docs/plans/2026-06-07-turn-tool-pipeline-rfc.md.)
    deliver_instruction_and_context(
        writer,
        shared,
        control_plane,
        resolved_instruction,
        context_buffer,
        &transition_result,
        should_prompt,
    )
    .await;

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
    let _ = state.session().set("turn_count", tc + 1);

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

/// Deliver the resolved instruction and any batched context turns for a turn.
///
/// Encodes three invariants (asserted by the `harness` tests):
/// - the instruction is delivered **once** and deduped against the last sent
///   (InstructionUpdate/Hybrid), or accumulated as a context frame
///   (ContextInjection);
/// - on-enter context and prompt from a phase transition join the same batch;
/// - delivery is `Immediate` (one `send_client_content`) or `Deferred` (queued in
///   `PendingContext` for the next user send), never a burst of isolated frames.
async fn deliver_instruction_and_context(
    writer: &Arc<dyn SessionWriter>,
    shared: &SharedState,
    control_plane: &ControlPlaneConfig,
    resolved_instruction: Option<String>,
    mut context_buffer: Vec<gemini_genai_rs::prelude::Content>,
    transition_result: &Option<TransitionResult>,
    mut should_prompt: bool,
) {
    // Instruction delivery (dedup against last sent).
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
                context_buffer.push(gemini_genai_rs::prelude::Content::model(instruction));
            }
        }
    }

    // Add on_enter_context content to the batch (if a phase transition produced it).
    if let Some(ref tr) = transition_result {
        if let Some(ref contents) = tr.context {
            context_buffer.extend(contents.iter().cloned());
        }
        if tr.prompt_on_enter {
            should_prompt = true;
        }
    }

    // Context delivery — Immediate (one atomic frame now) or Deferred (queued in
    // PendingContext, synchronized with the next user send by the DeferredWriter,
    // so context never arrives as isolated frames during silence).
    if !context_buffer.is_empty() || should_prompt {
        use crate::live::steering::ContextDelivery;
        match (&control_plane.context_delivery, &shared.pending_context) {
            (ContextDelivery::Deferred, Some(pending)) => {
                pending.extend(context_buffer);
                if should_prompt {
                    pending.set_prompt();
                }
            }
            _ => {
                if !context_buffer.is_empty() {
                    writer.send_client_content(context_buffer, false).await.ok();
                }
                if should_prompt {
                    writer.send_client_content(vec![], true).await.ok();
                }
            }
        }
    }
}

#[cfg(test)]
mod harness {
    //! Deterministic turn-lifecycle harness.
    //!
    //! Drives the real [`handle_turn_complete`] with a recording [`SessionWriter`]
    //! so the documented ordering "scars" become asserted invariants rather than
    //! comments — the safety net for the staged turn-pipeline refactor
    //! (`docs/plans/2026-06-07-turn-tool-pipeline-rfc.md`).

    use super::handle_turn_complete;
    use crate::live::callbacks::EventCallbacks;
    use crate::live::computed::ComputedRegistry;
    use crate::live::context_writer::PendingContext;
    use crate::live::events::LiveEvent;
    use crate::live::extractor::TurnExtractor;
    use crate::live::phase::PhaseMachine;
    use crate::live::processor::{ControlPlaneConfig, SharedState};
    use crate::live::steering::{ContextDelivery, SteeringMode};
    use crate::live::temporal::TemporalRegistry;
    use crate::live::transcript::TranscriptBuffer;
    use crate::live::watcher::WatcherRegistry;
    use crate::state::State;

    use gemini_genai_rs::prelude::{Content, FunctionResponse};
    use gemini_genai_rs::session::{SessionError, SessionWriter};

    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::broadcast;

    /// One observable wire write.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Write {
        Instruction(String),
        ClientContent { turns: usize, turn_complete: bool },
    }

    /// A `SessionWriter` that records the wire writes that matter for the scars.
    #[derive(Default)]
    struct RecordingWriter {
        log: Mutex<Vec<Write>>,
    }

    #[async_trait::async_trait]
    impl SessionWriter for RecordingWriter {
        async fn send_audio(&self, _: Vec<u8>) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_text(&self, _: String) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_tool_response(&self, _: Vec<FunctionResponse>) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_client_content(
            &self,
            turns: Vec<Content>,
            turn_complete: bool,
        ) -> Result<(), SessionError> {
            self.log.lock().push(Write::ClientContent {
                turns: turns.len(),
                turn_complete,
            });
            Ok(())
        }
        async fn send_video(&self, _: Vec<u8>) -> Result<(), SessionError> {
            Ok(())
        }
        async fn update_instruction(&self, instruction: String) -> Result<(), SessionError> {
            self.log.lock().push(Write::Instruction(instruction));
            Ok(())
        }
        async fn signal_activity_start(&self) -> Result<(), SessionError> {
            Ok(())
        }
        async fn signal_activity_end(&self) -> Result<(), SessionError> {
            Ok(())
        }
        async fn disconnect(&self) -> Result<(), SessionError> {
            Ok(())
        }
    }

    /// Fixture: the collaborators a turn-complete invocation needs, with the
    /// optional subsystems off so a test exercises one path at a time.
    struct Harness {
        rec: Arc<RecordingWriter>,
        writer: Arc<dyn SessionWriter>,
        shared: SharedState,
        state: State,
        transcript: TranscriptBuffer,
        tracker: HashMap<String, u32>,
        control: ControlPlaneConfig,
        callbacks: EventCallbacks,
        event_tx: broadcast::Sender<LiveEvent>,
    }

    impl Harness {
        fn new() -> Self {
            let rec = Arc::new(RecordingWriter::default());
            let writer: Arc<dyn SessionWriter> = rec.clone();
            let mut transcript = TranscriptBuffer::new();
            transcript.push_input("hello");
            transcript.push_output("hi");
            let (event_tx, _rx) = broadcast::channel(16);
            Self {
                rec,
                writer,
                shared: SharedState {
                    interrupted: AtomicBool::new(false),
                    resume_handle: Mutex::new(None),
                    last_instruction: Mutex::new(None),
                    pending_context: None,
                },
                state: State::new(),
                transcript,
                tracker: HashMap::new(),
                control: ControlPlaneConfig::default(),
                callbacks: EventCallbacks::default(),
                event_tx,
            }
        }

        async fn run_turn(&mut self) {
            let extractors: Vec<Arc<dyn TurnExtractor>> = vec![];
            let computed: Option<ComputedRegistry> = None;
            let phase: Option<tokio::sync::Mutex<PhaseMachine>> = None;
            let watchers: Option<WatcherRegistry> = None;
            let temporal: Option<Arc<TemporalRegistry>> = None;
            handle_turn_complete(
                &self.callbacks,
                &self.writer,
                &self.shared,
                &extractors,
                &self.state,
                &computed,
                &phase,
                &watchers,
                &temporal,
                &mut self.transcript,
                &mut self.tracker,
                &mut self.control,
                &self.event_tx,
            )
            .await;
        }

        fn writes(&self) -> Vec<Write> {
            self.rec.log.lock().clone()
        }
    }

    #[tokio::test]
    async fn turn_state_is_reset_and_count_incremented_and_callback_fires() {
        let mut h = Harness::new();
        let _ = h.state.set("turn:scratch", true);
        let _ = h.state.session().set("turn_count", 5u32);
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        h.callbacks.on_turn_complete = Some(Arc::new(move || {
            let flag = flag.clone();
            Box::pin(async move {
                flag.store(true, Ordering::SeqCst);
            })
        }));

        h.run_turn().await;

        assert_eq!(h.state.get::<bool>("turn:scratch"), None, "turn: cleared");
        assert_eq!(h.state.session().get::<u32>("turn_count"), Some(6));
        assert!(fired.load(Ordering::SeqCst), "on_turn_complete fired");
    }

    #[tokio::test]
    async fn instruction_is_sent_once_then_deduped() {
        // The "single resolved_instruction, sent once" + dedup scar.
        let mut h = Harness::new();
        h.control.steering_mode = SteeringMode::InstructionUpdate;
        h.callbacks.instruction_template = Some(Arc::new(|_| Some("SYSTEM".to_string())));

        h.run_turn().await;
        assert_eq!(h.writes(), vec![Write::Instruction("SYSTEM".into())]);

        // Same instruction next turn -> deduped, not resent.
        h.run_turn().await;
        assert_eq!(
            h.writes(),
            vec![Write::Instruction("SYSTEM".into())],
            "identical instruction must not be re-sent"
        );
    }

    #[tokio::test]
    async fn context_injection_delivers_instruction_as_one_batched_frame() {
        // ContextInjection steering: the instruction is a single context frame,
        // not an update_instruction and not a burst of frames.
        let mut h = Harness::new();
        h.control.steering_mode = SteeringMode::ContextInjection;
        h.callbacks.instruction_template = Some(Arc::new(|_| Some("CTX".to_string())));

        h.run_turn().await;

        assert_eq!(
            h.writes(),
            vec![Write::ClientContent {
                turns: 1,
                turn_complete: false
            }],
            "exactly one batched context frame, no update_instruction"
        );
    }

    #[tokio::test]
    async fn deferred_context_is_queued_not_sent() {
        // The deferred-context scar: context is queued for the next user send,
        // not emitted as an isolated frame during silence.
        let mut h = Harness::new();
        let pending = Arc::new(PendingContext::new());
        h.shared.pending_context = Some(pending.clone());
        h.control.steering_mode = SteeringMode::ContextInjection;
        h.control.context_delivery = ContextDelivery::Deferred;
        h.callbacks.instruction_template = Some(Arc::new(|_| Some("DEFERRED".to_string())));

        h.run_turn().await;

        assert!(
            h.writes().is_empty(),
            "nothing sent on the wire in Deferred mode"
        );
        assert_eq!(pending.drain_context().len(), 1, "context queued instead");
    }
}
