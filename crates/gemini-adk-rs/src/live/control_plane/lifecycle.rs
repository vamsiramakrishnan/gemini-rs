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
#[allow(
    clippy::too_many_arguments,
    reason = "turn-boundary stage entry point: each parameter is one control-plane subsystem; bundling them into a struct would just move the list (see the turn-pipeline RFC)"
)]
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

    // 4. Run extractors matching EveryTurn or Interval triggers. (Extracted so the
    // trigger-gating + interval-tracker bookkeeping is a named, harness-covered
    // unit — see `harness` below and docs/plans/2026-06-07-turn-tool-pipeline-rfc.md.)
    run_turn_extractors(
        extractors,
        transcript_buffer,
        state,
        callbacks,
        extraction_turn_tracker,
        event_tx,
    )
    .await;

    // 5. Recompute derived state
    if let Some(computed) = computed {
        computed.recompute(state);
    }

    // 6. Build transcript window snapshot for phase evaluation
    let transcript_window = transcript_buffer.snapshot_window(5);

    // Batched context buffer: all model-role context turns are accumulated here
    // and sent as a SINGLE send_client_content call, eliminating the burst of
    // separate WebSocket frames that can confuse the model or clash with user input.
    let mut context_buffer: Vec<gemini_genai_rs::prelude::Content> = Vec::new();
    // Whether to prompt the model after sending the batched context.
    let mut should_prompt = false;

    // 7. Evaluate phase transitions + compute navigation context. (Extracted so
    // the transition evaluation + target-prep retry + phase-state persistence is
    // a named, harness-covered unit — see `harness` below and
    // docs/plans/2026-06-07-turn-tool-pipeline-rfc.md.)
    //
    // Unified instruction composition: a fired transition seeds
    // `resolved_instruction`, which later steps (10/11) may amend/override, so it
    // is sent ONCE at the end rather than at each step.
    let PhaseOutcome {
        mut resolved_instruction,
        transition_result,
        transition_from,
        transition_to,
    } = evaluate_phase_transition(phase_machine, state, writer, &transcript_window).await;

    // 7c. Emit PhaseTransition LiveEvent (if a transition fired)
    if let (Some(from), Some(to)) = (&transition_from, &transition_to) {
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

    // 7d. Tool availability advisory (Phase 5). (Extracted so the active-tool
    // diffing + advisory projection is a named, harness-covered unit — see
    // `harness` below and docs/plans/2026-06-07-turn-tool-pipeline-rfc.md.)
    project_tool_advisory(
        transition_result.is_some() && control_plane.tool_advisory,
        phase_machine,
        state,
        &mut context_buffer,
    )
    .await;

    // 7e. Conversation repair (Phase 6). (Extracted so the needs evaluation +
    // nudge/escalate projection is a named, harness-covered unit — see `harness`
    // below and docs/plans/2026-06-07-turn-tool-pipeline-rfc.md.)
    should_prompt |= evaluate_repair(
        &mut control_plane.needs_fulfillment,
        phase_machine,
        state,
        &mut context_buffer,
    )
    .await;

    // 7f. Context injection steering (Phase 4). (Extracted so the modifier-based
    // steering projection is a named, harness-covered unit — see `harness` below
    // and docs/plans/2026-06-07-turn-tool-pipeline-rfc.md.)
    project_steering_context(
        control_plane.steering_mode,
        phase_machine,
        state,
        &mut context_buffer,
    )
    .await;

    // 7g. Flow governance. (Extracted so the re-latch + status publish + posture
    // /grounding/unmet projection + on-enter firing is a named, harness-covered
    // unit — see `harness` below and docs/plans/2026-06-07-turn-tool-pipeline-rfc.md.)
    govern_flow(&control_plane.flow, state, &mut context_buffer).await;

    // 8. Fire watchers from net state mutations since the cursor.
    if let (Some(watchers), Some(cursor)) = (watchers, pre_watcher_cursor) {
        let mutations = state.mutations_since(cursor);
        if !mutations.is_empty() {
            let (blocking, concurrent) = watchers.evaluate_mutations(&mutations, state, writer);
            for action in blocking {
                action.await;
            }
            for action in concurrent {
                tokio::spawn(action);
            }
        }
    }

    // 9. Check temporal patterns
    if let Some(temporal) = temporal {
        let event = SessionEvent::TurnComplete;
        for action in temporal.check_all(state, Some(&event), writer) {
            tokio::spawn(action);
        }
    }

    // 10. Instruction amendment (additive -- appends to the base instruction)
    // Only applies when there was NO phase transition (transition already includes modifiers)
    //
    // The base is the current phase's instruction when there is a phase machine,
    // and the connect-time system instruction when there is not. That fallback
    // is load-bearing rather than tidy: `Live::builder().instruction(..)` with
    // no phases is the ordinary shape of a session, and without it every
    // amendment in such a session was computed and then thrown away, because
    // `base` was `None` and the `if let` simply did not fire. Nothing reported
    // it — the callback ran, so it looked wired.
    //
    // The concrete casualty was `gemini-memory-rs`, which delivers its memory
    // map this way. Measured, that map takes a model's ability to name a
    // `recall_context` filter from 2% to 69%; in a no-phase session it was
    // never delivered, so the filters it exists to enable were being written
    // blind.
    if transition_result.is_none()
        && let Some(ref amendment_fn) = callbacks.instruction_amendment
        && let Some(amendment_text) = amendment_fn(state)
    {
        let base = if let Some(pm) = phase_machine {
            let pm_guard = pm.lock().await;
            pm_guard
                .current_phase()
                .map(|p| p.instruction.resolve_with_modifiers(state, &p.modifiers))
        } else {
            None
        };
        // Falling back to the session instruction, not to sending the
        // amendment alone: under `SteeringMode::InstructionUpdate` the
        // resolved instruction *replaces* the system instruction, so an
        // amendment on its own would delete the caller's prompt.
        if let Some(base_instruction) = base.or_else(|| control_plane.base_instruction.clone()) {
            resolved_instruction = Some(format!("{base_instruction}\n\n{amendment_text}"));
        }
    }

    // 11. Instruction template (full replacement -- escape hatch, overrides everything)
    if let Some(ref template) = callbacks.instruction_template
        && let Some(new_instruction) = template(state)
    {
        resolved_instruction = Some(new_instruction);
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

    // 18. Persist session state (Phase 7 -- fire and forget; a final
    // *synchronous* snapshot also runs on control-lane exit via
    // `final_drain`, so the last turn can't be lost to a spawned save racing
    // process shutdown).
    if let Some(ref persistence) = control_plane.persistence {
        let snapshot =
            build_snapshot(state, phase_machine, transcript_buffer, shared, tc + 1).await;
        let p = persistence.clone();
        let sid = control_plane
            .session_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        tokio::spawn(async move {
            if let Err(e) = p.save(&sid, &snapshot).await {
                tracing::warn!("Session persistence failed: {e}");
            }
        });
    }
}

/// Build a [`SessionSnapshot`](crate::live::persistence::SessionSnapshot) of
/// the current control-plane state (shared by the per-turn spawned save and
/// the synchronous final save on lane exit).
async fn build_snapshot(
    state: &State,
    phase_machine: &Option<tokio::sync::Mutex<PhaseMachine>>,
    transcript_buffer: &mut TranscriptBuffer,
    shared: &SharedState,
    turn_count: u32,
) -> crate::live::persistence::SessionSnapshot {
    let phase_name = if let Some(pm) = phase_machine {
        pm.lock().await.current().to_string()
    } else {
        String::new()
    };
    crate::live::persistence::SessionSnapshot {
        state: state.to_hashmap(),
        phase: phase_name,
        turn_count,
        transcript_summary: transcript_buffer.format_window(5),
        resume_handle: shared.resume_handle.lock().clone(),
        saved_at: {
            // Simple ISO 8601 timestamp without chrono dependency
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            format!("{}s", now.as_secs())
        },
    }
}

/// Graceful drain on control-lane exit.
///
/// Runs once, after the control lane's event channel closes (session
/// disconnected or router gone):
///
/// 1. Best-effort flush of any deferred context still queued in
///    [`PendingContext`](crate::live::context_writer::PendingContext) — under
///    `ContextDelivery::Deferred` these turns would otherwise be silently
///    dropped on disconnect.
/// 2. A final persistence snapshot, **awaited synchronously** (unlike the
///    per-turn spawn-and-forget save), so state accumulated since the last
///    turn boundary — or a last turn whose spawned save lost the race with
///    shutdown — is not lost.
pub(in crate::live) async fn final_drain(
    writer: &Arc<dyn SessionWriter>,
    shared: &SharedState,
    state: &State,
    phase_machine: &Option<tokio::sync::Mutex<PhaseMachine>>,
    transcript_buffer: &mut TranscriptBuffer,
    control_plane: &ControlPlaneConfig,
) {
    // 1. Flush deferred context (best effort; the session may already be gone).
    if let Some(ref pending) = control_plane.pending_context {
        let context = pending.drain_context();
        if !context.is_empty() {
            writer.send_client_content(context, false).await.ok();
        }
    }

    // 2. Final synchronous persistence snapshot.
    if let Some(ref persistence) = control_plane.persistence {
        let turn_count = state.session().get::<u32>("turn_count").unwrap_or(0);
        let snapshot =
            build_snapshot(state, phase_machine, transcript_buffer, shared, turn_count).await;
        let sid = control_plane
            .session_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        if let Err(e) = persistence.save(&sid, &snapshot).await {
            tracing::warn!("Final session persistence failed: {e}");
        }
    }
}

/// Project a tool-availability advisory into the context buffer.
///
/// When `enabled` (a phase transition fired and `tool_advisory` is on) and the
/// active phase's tool set differs from the one last advertised, persists the
/// new set to `active_tools` and pushes a "tools I have access to" advisory line.
/// Behavior-preserving lift of step 7d. No-op when disabled or unchanged.
async fn project_tool_advisory(
    enabled: bool,
    phase_machine: &Option<tokio::sync::Mutex<PhaseMachine>>,
    state: &State,
    context_buffer: &mut Vec<gemini_genai_rs::prelude::Content>,
) {
    if enabled && let Some(pm) = phase_machine {
        let machine = pm.lock().await;
        if let Some(tools) = machine.active_tools() {
            let prev_tools: Option<Vec<String>> = state.session().get("active_tools");
            let tools_vec: Vec<String> =
                tools.iter().map(std::string::ToString::to_string).collect();
            let changed = prev_tools.as_ref() != Some(&tools_vec);
            if changed {
                let _ = state.session().set("active_tools", tools_vec.clone());
                let tool_names = tools_vec.join(", ");
                context_buffer.push(gemini_genai_rs::prelude::Content::model(format!(
                    "In this phase, I have access to these tools: {tool_names}. \
                         I should only use these tools."
                )));
            }
        }
    }
}

/// Project context-injection steering modifiers into the context buffer.
///
/// Under `ContextInjection`/`Hybrid` steering, composes the active phase's
/// instruction modifiers into a steering context line. Behavior-preserving lift
/// of step 7f. No-op under `InstructionUpdate` steering or with no phase machine.
async fn project_steering_context(
    steering_mode: SteeringMode,
    phase_machine: &Option<tokio::sync::Mutex<PhaseMachine>>,
    state: &State,
    context_buffer: &mut Vec<gemini_genai_rs::prelude::Content>,
) {
    if matches!(
        steering_mode,
        SteeringMode::ContextInjection | SteeringMode::Hybrid
    ) && let Some(pm) = phase_machine
    {
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

/// Evaluate conversation repair for the current phase's `needs`.
///
/// When the active phase declares unmet `needs`, runs the needs tracker and
/// projects the outcome: a `Nudge` pushes a "still need to collect" context line
/// (and, on the first attempt, requests a prompt — returned as `true`); an
/// `Escalate` latches `repair:escalation` + `repair:unfulfilled` into state.
/// Behavior-preserving lift of step 7e. Returns whether the model should be
/// prompted after the batched context.
async fn evaluate_repair(
    needs_fulfillment: &mut Option<crate::live::needs::NeedsFulfillment>,
    phase_machine: &Option<tokio::sync::Mutex<PhaseMachine>>,
    state: &State,
    context_buffer: &mut Vec<gemini_genai_rs::prelude::Content>,
) -> bool {
    let mut should_prompt = false;
    if let Some(needs_tracker) = needs_fulfillment
        && let Some(pm) = phase_machine
    {
        let machine = pm.lock().await;
        let phase_name = machine.current().to_string();
        if let Some(phase) = machine.current_phase()
            && !phase.needs.is_empty()
        {
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
    should_prompt
}

/// Re-latch the governed flow for a turn and project its status.
///
/// Re-evaluates the marking, publishes `flow:done` / `flow:active`, pushes
/// active-step postures and grounding lines into the context buffer, surfaces
/// unmet requirements as a repair line, and fires on-enter actions for steps
/// that just became active. Behavior-preserving lift of step 7g. No-op when no
/// flow is governing the session.
///
/// The monitor is shared with the [`LiveHandle`](crate::live::LiveHandle)
/// (`explain`/`why_blocked` snapshots), so the lock is held only for the
/// synchronous re-latch + projection; on-enter actions (which may await an
/// inline agent) fire after the guard is dropped.
async fn govern_flow(
    flow: &Option<crate::flow::SharedFlowMonitor>,
    state: &State,
    context_buffer: &mut Vec<gemini_genai_rs::prelude::Content>,
) {
    if let Some(mon_arc) = flow {
        let enter_actions = {
            let mut mon = mon_arc.lock();
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
            // Collect on-enter actions for steps that just became active, to
            // fire below without holding the lock across an await.
            mon.take_newly_active(state)
                .into_iter()
                .filter_map(|id| mon.enter_action(&id).cloned().map(|a| (id, a)))
                .collect::<Vec<_>>()
        };
        // Fire on_enter actions for steps that just became active. `Call`
        // actions resolve inline; `Dispatch`/`Background` run detached.
        for (id, action) in enter_actions {
            action.fire(&id, state).await;
        }
    }
}

/// Result of evaluating phase transitions for a turn.
///
/// A fired transition seeds `resolved_instruction` (the new phase's instruction)
/// and records the `from`/`to` phase names plus the full [`TransitionResult`] for
/// downstream steps (event emission, OnPhaseChange extractors, on-enter context).
struct PhaseOutcome {
    resolved_instruction: Option<String>,
    transition_result: Option<TransitionResult>,
    transition_from: Option<String>,
    transition_to: Option<String>,
}

/// Evaluate phase transitions and compute navigation context for a turn.
///
/// Runs target preparation when a guarded transition is blocked only by missing
/// required state (re-evaluating once after prep), fires the transition if ready,
/// persists the resulting phase + its `needs`/`requires` to state, and always
/// refreshes the stored navigation context. Behavior-preserving lift of step 7.
async fn evaluate_phase_transition(
    phase_machine: &Option<tokio::sync::Mutex<PhaseMachine>>,
    state: &State,
    writer: &Arc<dyn SessionWriter>,
    transcript_window: &crate::live::transcript::TranscriptWindow,
) -> PhaseOutcome {
    let mut resolved_instruction: Option<String> = None;
    let mut transition_result: Option<TransitionResult> = None;
    let mut transition_from: Option<String> = None;
    let mut transition_to: Option<String> = None;

    if let Some(pm) = phase_machine {
        let mut machine = pm.lock().await;

        // 7a. Evaluate transitions and run target preparations when a guarded
        // transition is blocked only by missing required state.
        let mut evaluation = machine.evaluate_for_transition(state);
        if let Some(TransitionEvaluation::Blocked { target, .. }) = &evaluation
            && machine.prepare_target(target, state, writer).await
        {
            evaluation = machine.evaluate_for_transition(state);
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
                .transition(&target, state, writer, turn, trigger, transcript_window)
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

    PhaseOutcome {
        resolved_instruction,
        transition_result,
        transition_from,
        transition_to,
    }
}

/// Run the extractors eligible to fire on a plain turn boundary.
///
/// Selects extractors whose trigger is `EveryTurn`, or `Interval(n)` when at
/// least `n` turns have elapsed since they last ran, runs them via
/// [`run_extractors`], then advances the interval tracker for any interval
/// extractor that fired. `AfterToolCall` / `OnPhaseChange` /
/// `OnGenerationComplete` extractors are owned by their respective stages.
async fn run_turn_extractors(
    extractors: &[Arc<dyn TurnExtractor>],
    transcript_buffer: &mut TranscriptBuffer,
    state: &State,
    callbacks: &EventCallbacks,
    extraction_turn_tracker: &mut std::collections::HashMap<String, u32>,
    event_tx: &tokio::sync::broadcast::Sender<LiveEvent>,
) {
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
}

/// The suppression key for a steering turn: its text, when it is only text.
///
/// A turn carrying anything else is never suppressed — equality of the rendered
/// text is not equality of the content, and silently dropping a turn because its
/// caption matched would lose the payload.
fn steering_key(content: &gemini_genai_rs::prelude::Content) -> Option<String> {
    let mut text = String::new();
    for part in &content.parts {
        match part {
            gemini_genai_rs::prelude::Part::Text { text: t } => text.push_str(t),
            _ => return None,
        }
    }
    Some(text)
}

/// Deliver the resolved instruction and any batched context turns for a turn.
///
/// Encodes four invariants (asserted by the `harness` tests):
/// - the instruction is delivered **once** and deduped against the last sent
///   (InstructionUpdate/Hybrid), or accumulated as a context frame
///   (ContextInjection);
/// - on-enter context and prompt from a phase transition join the same batch;
/// - a steering line identical to the previous turn's is suppressed, because the
///   conversation it is appended to has no way to retract the earlier copy;
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
    if let Some(tr) = transition_result {
        if let Some(ref contents) = tr.context {
            context_buffer.extend(contents.iter().cloned());
        }
        if tr.prompt_on_enter {
            should_prompt = true;
        }
    }

    // Repeat suppression, against the previous turn's steering.
    //
    // `send_client_content` appends to the server-side conversation, and nothing
    // can retract what it appends. So a step that stays active across N turns
    // used to deposit N verbatim copies of its imperative posture into history,
    // in the model's own voice — and by the time the step latched, the stale
    // orders outnumbered the live one. Re-sending unchanged steering does not
    // reinforce it; it accumulates directives whose preconditions have expired.
    //
    // Keyed against the previous turn only, so steering that oscillates because
    // its underlying condition genuinely oscillated is still delivered, and a
    // step that advances is never muted.
    let keys: Vec<Option<String>> = context_buffer.iter().map(steering_key).collect();
    {
        let mut last = shared.last_context.lock();
        let mut kept = Vec::with_capacity(context_buffer.len());
        for (content, key) in std::mem::take(&mut context_buffer)
            .into_iter()
            .zip(keys.iter())
        {
            let repeated = key.as_ref().is_some_and(|k| last.iter().any(|p| p == k));
            if !repeated {
                kept.push(content);
            }
        }
        // What the model's standing steering *is* this turn, not what was newly
        // sent: storing the post-suppression set would let a line suppressed on
        // one turn be re-sent on the next, which is the behaviour being fixed.
        *last = keys.into_iter().flatten().collect();
        context_buffer = kept;
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
    use crate::flow::{Enforcement, Flow, FlowMonitor, Guard};
    use crate::live::callbacks::EventCallbacks;
    use crate::live::computed::ComputedRegistry;
    use crate::live::context_writer::PendingContext;
    use crate::live::events::LiveEvent;
    use crate::live::extractor::{ExtractionTrigger, TurnExtractor};
    use crate::live::needs::{NeedsFulfillment, RepairConfig};
    use crate::live::phase::{InstructionModifier, Phase, PhaseMachine, Transition};
    use crate::live::processor::{ControlPlaneConfig, SharedState};
    use crate::live::steering::{ContextDelivery, SteeringMode};
    use crate::live::temporal::TemporalRegistry;
    use crate::live::transcript::TranscriptBuffer;
    use crate::live::transcript::TranscriptTurn;
    use crate::live::watcher::WatcherRegistry;
    use crate::llm::LlmError;
    use crate::state::State;

    use gemini_genai_rs::prelude::{Content, FunctionResponse};
    use gemini_genai_rs::session::{SessionError, SessionWriter};

    use async_trait::async_trait;
    use parking_lot::Mutex;
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
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
        /// Flattened text of each `send_client_content` batch, in send order.
        /// The batch *is* the steering, so the order within it is a contract.
        batches: Mutex<Vec<Vec<String>>>,
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
            self.batches.lock().push(
                turns
                    .iter()
                    .map(|t| {
                        t.parts
                            .iter()
                            .filter_map(|p| match p {
                                gemini_genai_rs::prelude::Part::Text { text } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .collect(),
            );
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

    /// A `TurnExtractor` that returns a fixed value with a fixed trigger — enough
    /// to assert the turn pipeline fills a slot from the turn.
    struct FixedExtractor {
        name: &'static str,
        value: Value,
        trigger: ExtractionTrigger,
    }

    #[async_trait]
    impl TurnExtractor for FixedExtractor {
        fn name(&self) -> &str {
            self.name
        }
        fn window_size(&self) -> usize {
            1
        }
        fn trigger(&self) -> ExtractionTrigger {
            self.trigger.clone()
        }
        async fn extract(&self, _window: &[TranscriptTurn]) -> Result<Value, LlmError> {
            Ok(self.value.clone())
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
        extractors: Vec<Arc<dyn TurnExtractor>>,
        phase: Option<tokio::sync::Mutex<PhaseMachine>>,
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
                    barge_in: Mutex::new(tokio_util::sync::CancellationToken::new()),
                    resume_handle: Mutex::new(None),
                    last_instruction: Mutex::new(None),
                    last_context: Mutex::new(Vec::new()),
                    pending_context: None,
                    delivery: crate::live::processor::DeliveryConfig::default(),
                    dropped: crate::live::processor::DroppedFrames::default(),
                    redactor: None,
                },
                state: State::new(),
                transcript,
                tracker: HashMap::new(),
                control: ControlPlaneConfig::default(),
                callbacks: EventCallbacks::default(),
                extractors: vec![],
                phase: None,
                event_tx,
            }
        }

        async fn run_turn(&mut self) {
            let computed: Option<ComputedRegistry> = None;
            let watchers: Option<WatcherRegistry> = None;
            let temporal: Option<Arc<TemporalRegistry>> = None;
            handle_turn_complete(
                &self.callbacks,
                &self.writer,
                &self.shared,
                &self.extractors,
                &self.state,
                &computed,
                &self.phase,
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

        /// The steering batches sent this run, each as its ordered turn texts.
        fn batches(&self) -> Vec<Vec<String>> {
            self.rec.batches.lock().clone()
        }
    }

    /// An amendment must reach the model in a session with no phase machine.
    ///
    /// This is the ordinary shape of a Live session —
    /// `Live::builder().instruction(..)` and no phases — and the amendment was
    /// silently dropped in it: the base came only from `current_phase()`, so
    /// with no phase there was no base, and the `if let` did not fire. The
    /// callback still ran, so from outside it looked wired.
    ///
    /// Asserted on the wire rather than on a variable, because "computed" was
    /// never the thing in doubt.
    #[tokio::test]
    async fn an_amendment_reaches_the_model_without_a_phase_machine() {
        let mut h = Harness::new();
        h.control.base_instruction = Some("You are a companion.".to_string());
        h.callbacks.instruction_amendment =
            Some(Arc::new(|_state| Some("Known values: coffee.".to_string())));

        h.run_turn().await;

        let sent: Vec<String> = h
            .writes()
            .into_iter()
            .filter_map(|w| match w {
                Write::Instruction(text) => Some(text),
                Write::ClientContent { .. } => None,
            })
            .collect();
        assert_eq!(
            sent.len(),
            1,
            "expected exactly one instruction update, got {sent:?}"
        );
        // Composed onto the session instruction, not replacing it: under
        // `InstructionUpdate` the resolved instruction *is* the system
        // instruction, so sending the amendment alone would delete the
        // caller's prompt.
        assert!(
            sent[0].contains("You are a companion."),
            "the caller's own instruction was dropped: {:?}",
            sent[0]
        );
        assert!(
            sent[0].contains("Known values: coffee."),
            "the amendment never reached the model: {:?}",
            sent[0]
        );
    }

    /// With no session instruction there is nothing to compose onto, and the
    /// amendment must not be sent alone — that would replace the model's
    /// instruction with a fragment.
    #[tokio::test]
    async fn an_amendment_alone_never_replaces_the_system_instruction() {
        let mut h = Harness::new();
        h.control.base_instruction = None;
        h.callbacks.instruction_amendment =
            Some(Arc::new(|_state| Some("Known values: coffee.".to_string())));

        h.run_turn().await;

        assert!(
            !h.writes()
                .iter()
                .any(|w| matches!(w, Write::Instruction(_))),
            "an amendment with no base was sent as the whole instruction"
        );
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

    #[tokio::test]
    async fn every_turn_extractor_fills_a_slot() {
        // The extractor stage: an EveryTurn extractor runs on a turn boundary and
        // its result (plus auto-flattened fields) lands in State.
        let mut h = Harness::new();
        h.extractors = vec![Arc::new(FixedExtractor {
            name: "Order",
            value: json!({ "item": "latte" }),
            trigger: ExtractionTrigger::EveryTurn,
        })];

        h.run_turn().await;

        assert_eq!(
            h.state.get::<Value>("Order"),
            Some(json!({ "item": "latte" })),
            "extractor value stored under its name"
        );
        assert_eq!(
            h.state.get::<String>("item").as_deref(),
            Some("latte"),
            "auto-flattened field promoted to a slot"
        );
    }

    #[tokio::test]
    async fn interval_extractor_respects_its_cadence() {
        // Interval(2) runs at turn 0, is skipped at turn 1, runs again at turn 2 —
        // the interval-tracker bookkeeping moved into `run_turn_extractors`.
        let mut h = Harness::new();
        h.extractors = vec![Arc::new(FixedExtractor {
            name: "Order",
            value: json!({ "n": 1 }),
            trigger: ExtractionTrigger::Interval(2),
        })];

        // Turn 0: runs (last seen = 0, current = 0, 0 - 0 >= 2 is false)... so it
        // does NOT run at turn 0. First eligible turn is when current - last >= 2.
        h.run_turn().await; // turn_count 0 -> 1
        assert!(!h.state.contains("Order"), "skipped at turn 0 (0 - 0 < 2)");

        h.run_turn().await; // turn_count 1 -> 2
        assert!(!h.state.contains("Order"), "skipped at turn 1 (1 - 0 < 2)");

        h.run_turn().await; // turn_count 2 -> 3, 2 - 0 >= 2 -> runs
        assert_eq!(h.state.get::<Value>("Order"), Some(json!({ "n": 1 })));
    }

    #[tokio::test]
    async fn a_turn_advances_the_phase_when_the_guard_is_satisfied() {
        // The phase stage: a guarded transition fires on a turn boundary, the
        // machine advances, and the new phase is persisted to state.
        let mut h = Harness::new();
        let mut greeting = Phase::new("greeting", "Say hello");
        greeting.transitions.push(Transition {
            target: "main".into(),
            guard: Arc::new(|s| s.get::<bool>("ready").unwrap_or(false)),
            description: None,
        });
        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);
        machine.add_phase(Phase::new("main", "Main phase"));
        h.phase = Some(tokio::sync::Mutex::new(machine));

        // Guard not yet satisfied -> stays in greeting.
        h.run_turn().await;
        assert_eq!(h.phase.as_ref().unwrap().lock().await.current(), "greeting");

        // Satisfy the guard -> next turn advances to main.
        let _ = h.state.set("ready", true);
        h.run_turn().await;
        assert_eq!(h.phase.as_ref().unwrap().lock().await.current(), "main");
        assert_eq!(
            h.state.session().get::<String>("phase").as_deref(),
            Some("main"),
            "new phase persisted to state"
        );
    }

    /// Phases and flows are **independent, additive** steering, not one lowered
    /// onto the other, and both reach the model on the same turn.
    ///
    /// The documentation claimed `Flow` "lowers onto" the phase machine, which
    /// the control plane does not do: `phase_machine` and `flow` are separate
    /// `Option`s and step 7f (phase steering) and 7g (flow governance) both
    /// append to one `context_buffer`. Configure both and the model receives
    /// both, in this order:
    ///
    /// ```text
    ///   7d tool advisory
    ///   7e repair nudge
    ///   7f phase steering context   (modifiers, under ContextInjection)
    ///   7g flow posture → ground → unmet requirements
    ///      resolved phase instruction  (last, under ContextInjection)
    /// ```
    ///
    /// The two run on **different cadences**, which is what keeps them from
    /// fighting: a flow posture is re-projected every turn while the phase
    /// instruction is seeded only when a transition fires. On a quiet turn the
    /// model therefore hears the flow and not the phase.
    #[tokio::test]
    async fn a_quiet_turn_carries_the_flow_posture_and_no_phase_instruction() {
        let flow = Flow::new()
            .step("collect")
            .posture("FLOW-POSTURE")
            .done(Guard::is_true("collected"))
            .build()
            .expect("valid flow");

        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(Phase::new("greeting", "PHASE-INSTRUCTION"));

        let mut h = Harness::new();
        h.control.steering_mode = SteeringMode::ContextInjection;
        h.control.flow = Some(FlowMonitor::new(flow, Enforcement::Observe).into_shared());
        h.phase = Some(tokio::sync::Mutex::new(machine));

        h.run_turn().await;

        let sent: Vec<String> = h.batches().into_iter().flatten().collect();
        assert!(
            sent.iter().any(|t| t.contains("FLOW-POSTURE")),
            "the active step's posture is projected every turn: {sent:?}"
        );
        assert!(
            !sent.iter().any(|t| t.contains("PHASE-INSTRUCTION")),
            "a phase instruction is seeded by a transition, not re-sent on \
             every turn — re-sending it would churn the model's framing: {sent:?}"
        );
    }

    /// A posture that has not changed is projected **once**, not once per turn.
    ///
    /// Every projection is a permanent `model`-role turn in the server-side
    /// conversation: `send_client_content` appends, and nothing can retract it.
    /// So a step that stays active across N turns used to deposit N verbatim
    /// copies of its imperative into history, in the model's own voice.
    ///
    /// That is what made the governed collections call re-ask for card digits
    /// after it had already verified them. `verify`'s posture ("Ask for the last
    /// four digits… Do not discuss the account… until that returns verified")
    /// went in on every turn verification took, so by the time the step latched
    /// the history held several self-attributed orders to ask, against a single
    /// later one to move on. The model followed the majority.
    ///
    /// The instruction channel already deduped against `last_instruction`, and
    /// the tool advisory already deduped against `active_tools` — the two
    /// channels carrying standing behavioural directives were the two without
    /// it.
    #[tokio::test]
    async fn an_unchanged_posture_is_projected_once_not_once_per_turn() {
        let flow = Flow::new()
            .step("verify")
            .posture("Ask for the last four digits.")
            .done(Guard::is_true("identity_verified"))
            .build()
            .expect("valid flow");

        let mut h = Harness::new();
        h.control.flow = Some(FlowMonitor::new(flow, Enforcement::Observe).into_shared());

        // Four turns of the caller stalling before the digits arrive.
        for _ in 0..4 {
            h.run_turn().await;
        }

        let projections = h
            .batches()
            .into_iter()
            .flatten()
            .filter(|t| t.contains("Ask for the last four digits."))
            .count();
        assert_eq!(
            projections, 1,
            "the posture is unchanged across all four turns, so it belongs in \
             the conversation once; {projections} copies is {projections} \
             standing orders the model must weigh against whatever comes next"
        );
    }

    /// Suppression is against the *previous* turn, not for all time: when the
    /// step advances, the new posture must reach the model.
    ///
    /// The failure mode this guards is a dedup that mutes too much — a flow
    /// whose steering goes silent after the first turn is worse than one that
    /// repeats itself, because nothing downstream reports it.
    #[tokio::test]
    async fn a_changed_posture_still_reaches_the_model() {
        let flow = Flow::new()
            .step("verify")
            .posture("Ask for the last four digits.")
            .done(Guard::is_true("identity_verified"))
            .step("disclose")
            .after("verify")
            .posture("Read the disclosure.")
            .done(Guard::is_true("disclosure_given"))
            .build()
            .expect("valid flow");

        let mut h = Harness::new();
        h.control.flow = Some(FlowMonitor::new(flow, Enforcement::Observe).into_shared());

        h.run_turn().await;
        h.run_turn().await;
        let _ = h.state.set("identity_verified", true);
        h.run_turn().await;

        let sent: Vec<String> = h.batches().into_iter().flatten().collect();
        assert_eq!(
            sent.iter()
                .filter(|t| t.contains("Ask for the last four digits."))
                .count(),
            1,
            "the verify posture repeated: {sent:?}"
        );
        assert_eq!(
            sent.iter()
                .filter(|t| t.contains("Read the disclosure."))
                .count(),
            1,
            "the step advanced and its posture never reached the model: {sent:?}"
        );
    }

    /// On a **transition** turn both do reach the model, and the phase
    /// instruction lands after the flow posture — nearest the user's next turn,
    /// so the phase persona is the most recent framing the model reads.
    ///
    /// Pinned because it is a real precedence decision that nothing else states.
    #[tokio::test]
    async fn on_a_transition_the_phase_instruction_follows_the_flow_posture() {
        let flow = Flow::new()
            .step("collect")
            .posture("FLOW-POSTURE")
            .done(Guard::is_true("collected"))
            .build()
            .expect("valid flow");

        let mut machine = PhaseMachine::new("greeting");
        let mut greeting = Phase::new("greeting", "GREETING-INSTRUCTION");
        greeting.transitions = vec![Transition {
            target: "main".into(),
            guard: Arc::new(|s: &State| s.get::<bool>("advance").unwrap_or(false)),
            description: None,
        }];
        machine.add_phase(greeting);
        machine.add_phase(Phase::new("main", "PHASE-INSTRUCTION"));

        let mut h = Harness::new();
        h.control.steering_mode = SteeringMode::ContextInjection;
        h.control.flow = Some(FlowMonitor::new(flow, Enforcement::Observe).into_shared());
        h.phase = Some(tokio::sync::Mutex::new(machine));
        let _ = h.state.set("advance", true);

        h.run_turn().await;

        let batches = h.batches();
        let batch = batches
            .iter()
            .find(|b| b.iter().any(|t| t.contains("PHASE-INSTRUCTION")))
            .unwrap_or_else(|| panic!("the new phase's instruction must be sent: {batches:?}"));

        let posture_at = batch.iter().position(|t| t.contains("FLOW-POSTURE"));
        let instruction_at = batch
            .iter()
            .position(|t| t.contains("PHASE-INSTRUCTION"))
            .expect("present by construction");

        if let Some(posture_at) = posture_at {
            assert!(
                posture_at < instruction_at,
                "the phase instruction must land after the flow posture, so the \
                 phase framing is the most recent thing the model reads: {batch:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_turn_publishes_governed_flow_status() {
        // The flow stage: a turn re-latches the marking and publishes
        // flow:active / flow:done to state.
        let flow = Flow::new()
            .step("greet")
            .posture("Greet the caller.")
            .done(Guard::is_true("greeted"))
            .step("end")
            .after("greet")
            .terminal()
            .build()
            .expect("valid flow");
        let mut h = Harness::new();
        h.control.flow = Some(FlowMonitor::new(flow, Enforcement::Observe).into_shared());

        // Not yet greeted -> greet is the active step, nothing done.
        h.run_turn().await;
        assert_eq!(
            h.state.get::<Vec<String>>("flow:active"),
            Some(vec!["greet".to_string()])
        );
        assert_eq!(h.state.get::<Vec<String>>("flow:done"), Some(vec![]));

        // Complete the step -> next turn latches it done.
        let _ = h.state.set("greeted", true);
        h.run_turn().await;
        assert!(
            h.state
                .get::<Vec<String>>("flow:done")
                .unwrap_or_default()
                .contains(&"greet".to_string()),
            "completed step latched done"
        );
    }

    #[tokio::test]
    async fn shared_monitor_snapshot_observes_control_lane_progress() {
        // The handle path: a clone of the shared monitor (what `LiveHandle`
        // holds) answers `why_blocked` against the marking the control lane
        // advances — without the two ever fighting over ownership.
        let flow = Flow::new()
            .step("verify")
            .allow(["lookup_account"])
            .done(Guard::is_true("identity_verified"))
            .step("pay")
            .after("verify")
            .allow(["charge_card"])
            .done(Guard::called_ok("charge_card"))
            .step("end")
            .after("pay")
            .terminal()
            .build()
            .expect("valid flow");
        let shared = FlowMonitor::new(flow, Enforcement::Enforce).into_shared();
        let mut h = Harness::new();
        h.control.flow = Some(shared.clone());

        // Before verification, the snapshot reports charge_card blocked.
        let ex = shared.lock().why_blocked(&h.state);
        assert!(ex.active.contains(&"verify".to_string()));
        assert!(ex.blocked_tools.contains_key("charge_card"));

        // The control lane latches `verify` on a turn; the external snapshot
        // sees the progress: `pay` is active and charge_card is admitted.
        let _ = h.state.set("identity_verified", true);
        h.run_turn().await;
        let ex = shared.lock().why_blocked(&h.state);
        assert!(ex.active.contains(&"pay".to_string()));
        assert!(ex.allowed_tools.contains(&"charge_card".to_string()));
    }

    #[tokio::test]
    async fn repair_nudges_then_escalates_when_a_need_stays_unmet() {
        // The repair stage: while the active phase's `needs` go unmet, repair
        // nudges, then latches `repair:escalation` once the threshold is crossed.
        let mut gather = Phase::new("gather", "Collect the customer id");
        gather.needs = vec!["customer_id".to_string()];
        let mut machine = PhaseMachine::new("gather");
        machine.add_phase(gather);

        let mut h = Harness::new();
        h.phase = Some(tokio::sync::Mutex::new(machine));
        h.control.needs_fulfillment = Some(NeedsFulfillment::new(
            RepairConfig::new().nudge_after(1).escalate_after(2),
        ));

        // Turn 1: stall count 1 -> nudge, no escalation yet.
        h.run_turn().await;
        assert_eq!(
            h.state.get::<bool>("repair:escalation"),
            None,
            "no escalation after one stall"
        );

        // Turn 2: stall count 2 -> escalate, signal latched into state.
        h.run_turn().await;
        assert_eq!(h.state.get::<bool>("repair:escalation"), Some(true));
        assert_eq!(
            h.state.get::<Vec<String>>("repair:unfulfilled"),
            Some(vec!["customer_id".to_string()])
        );
    }

    #[tokio::test]
    async fn phase_transition_advertises_the_new_tool_set() {
        // The tool-advisory stage: when a transition changes the active tool set,
        // the new set is persisted to active_tools (and advertised).
        let mut greeting = Phase::new("greeting", "Say hello");
        greeting.transitions.push(Transition {
            target: "main".into(),
            guard: Arc::new(|s| s.get::<bool>("ready").unwrap_or(false)),
            description: None,
        });
        let mut main = Phase::new("main", "Main phase");
        main.tools_enabled = Some(vec!["search".to_string()]);
        let mut machine = PhaseMachine::new("greeting");
        machine.add_phase(greeting);
        machine.add_phase(main);

        let mut h = Harness::new();
        h.phase = Some(tokio::sync::Mutex::new(machine));
        // tool_advisory defaults on.
        let _ = h.state.set("ready", true);

        h.run_turn().await;

        assert_eq!(
            h.state.session().get::<Vec<String>>("active_tools"),
            Some(vec!["search".to_string()]),
            "new phase's tool set advertised + persisted"
        );
    }

    #[tokio::test]
    async fn final_drain_flushes_deferred_context_and_persists_synchronously() {
        use crate::live::persistence::{MemoryPersistence, SessionPersistence};

        let mut h = Harness::new();

        // Deferred context still queued when the lane exits…
        let pending = Arc::new(PendingContext::new());
        pending.extend(vec![Content::model("queued context")]);
        h.shared.pending_context = Some(pending.clone());
        h.control.pending_context = Some(pending.clone());
        h.control.context_delivery = ContextDelivery::Deferred;

        // …and a persistence backend that must be hit synchronously.
        let p = Arc::new(MemoryPersistence::new());
        h.control.persistence = Some(p.clone());
        h.control.session_id = Some("drain-session".into());
        let _ = h.state.session().set("turn_count", 7u32);

        super::final_drain(
            &h.writer,
            &h.shared,
            &h.state,
            &h.phase,
            &mut h.transcript,
            &h.control,
        )
        .await;

        // The queued context was flushed as one frame (not dropped).
        assert_eq!(
            h.writes(),
            vec![Write::ClientContent {
                turns: 1,
                turn_complete: false
            }],
            "deferred context must be flushed on lane exit"
        );
        assert!(pending.drain_context().is_empty(), "queue fully drained");

        // The final snapshot was awaited before final_drain returned.
        let snap = p
            .load("drain-session")
            .await
            .unwrap()
            .expect("final snapshot must be persisted synchronously");
        assert_eq!(snap.turn_count, 7);
    }

    #[tokio::test]
    async fn context_injection_projects_phase_steering_modifiers() {
        // The steering stage: under ContextInjection, the active phase's
        // instruction modifiers are projected as one steering context frame.
        let mut phase = Phase::new("main", "Main phase");
        phase.modifiers = vec![InstructionModifier::StateAppend(vec!["mood".to_string()])];
        let mut machine = PhaseMachine::new("main");
        machine.add_phase(phase);

        let mut h = Harness::new();
        h.phase = Some(tokio::sync::Mutex::new(machine));
        h.control.steering_mode = SteeringMode::ContextInjection;
        let _ = h.state.set("mood", "calm");

        h.run_turn().await;

        assert_eq!(
            h.writes(),
            vec![Write::ClientContent {
                turns: 1,
                turn_complete: false
            }],
            "steering modifiers delivered as one batched context frame"
        );
    }
}
