//! Control lane main loop — dispatches ControlEvents to handlers.

use std::sync::Arc;
use std::time::Duration;

use std::sync::atomic::Ordering;

use gemini_genai_rs::session::SessionWriter;

use crate::state::State;
use crate::tool::ToolDispatcher;

use crate::live::background_tool::BackgroundToolTracker;
use crate::live::callbacks::EventCallbacks;
use crate::live::computed::ComputedRegistry;
use crate::live::events::LiveEvent;
use crate::live::extractor::{ExtractionTrigger, TurnExtractor};
use crate::live::phase::PhaseMachine;
use crate::live::processor::{ControlEvent, ControlPlaneConfig, SharedState};
use crate::live::temporal::TemporalRegistry;
use crate::live::transcript::TranscriptBuffer;
use crate::live::watcher::WatcherRegistry;

use super::dispatch_callback;
use super::extractors::run_extractors_with_window;
use super::lifecycle::{final_drain, handle_turn_complete};
use super::tool_gate::ToolGate;
use super::tool_handler::handle_tool_calls;

/// Control lane processor -- handles lifecycle events, tool dispatch,
/// transcript accumulation, extractors, phases, watchers.
///
/// TranscriptBuffer is owned exclusively -- no Arc<Mutex<>> needed.
#[allow(
    clippy::too_many_arguments,
    reason = "control-lane spawn site: parameters are the owned subsystem handles transferred onto the lane task"
)]
pub(in crate::live) async fn run_control_lane(
    mut rx: tokio::sync::mpsc::Receiver<ControlEvent>,
    completion_tx: tokio::sync::mpsc::WeakSender<ControlEvent>,
    callbacks: Arc<EventCallbacks>,
    dispatcher: Option<Arc<ToolDispatcher>>,
    writer: Arc<dyn SessionWriter>,
    shared: Arc<SharedState>,
    extractors: Vec<Arc<dyn TurnExtractor>>,
    state: State,
    computed: Option<ComputedRegistry>,
    phase_machine: Option<tokio::sync::Mutex<PhaseMachine>>,
    watchers: Option<WatcherRegistry>,
    temporal: Option<Arc<TemporalRegistry>>,
    background_tracker: Option<Arc<BackgroundToolTracker>>,
    execution_modes: std::collections::HashMap<
        String,
        crate::live::background_tool::ToolExecutionMode,
    >,
    mut control_plane: ControlPlaneConfig,
    event_tx: tokio::sync::broadcast::Sender<LiveEvent>,
) {
    // TranscriptBuffer is exclusively owned by the control lane -- no mutex.
    let mut transcript_buffer = TranscriptBuffer::new();

    // Middleware chain for tool-lifecycle hooks (shared, cheap Arc clone).
    let middleware = control_plane.middleware.clone();

    // Track which turn each interval-based extractor last ran on.
    let mut extraction_turn_tracker: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();

    // The single gate where completed tools advance the governed flow (#7).
    // Persists across tool-call events so inline and (later) background
    // completions dedupe by call_id.
    let mut tool_gate = ToolGate::new();

    // Accumulated transcript text for the current turn, used to synthesize the
    // `is_final = true` transcript callbacks at the turn boundary.
    //
    // Transcript finalization semantics (standard ASR partial/final pattern):
    //   - Per-chunk callbacks fire on the FAST lane with `is_final = false`.
    //   - At TurnComplete the control lane fires a SINGLE `is_final = true`
    //     callback delivering the full accumulated text for that turn.
    // Consumers that only want finals key on the bool. We accumulate here in the
    // exclusively-owned control lane (rather than reading from `transcript_buffer`)
    // because `TranscriptBuffer::end_turn` drains the current-turn text inside
    // `handle_turn_complete` before we could read it back.
    let mut accumulated_input = String::new();
    let mut accumulated_output = String::new();

    while let Some(event) = rx.recv().await {
        match event {
            // -- Transcript accumulation (exclusive to control lane) --
            ControlEvent::InputTranscript(text) => {
                transcript_buffer.push_input(&text);
                accumulated_input.push_str(&text);
            }
            ControlEvent::OutputTranscript(text) => {
                transcript_buffer.push_output(&text);
                accumulated_output.push_str(&text);
            }

            ControlEvent::ToolCall(calls) => {
                // Snapshot the current barge-in token: the router cancels it
                // on `Interrupted`, letting inline dispatch race the user's
                // interruption instead of blocking the lane behind a slow tool.
                let barge_in = shared.barge_in.lock().clone();
                handle_tool_calls(
                    calls,
                    &callbacks,
                    &dispatcher,
                    &writer,
                    &state,
                    &phase_machine,
                    &mut transcript_buffer,
                    &execution_modes,
                    &background_tracker,
                    &extractors,
                    &middleware,
                    &control_plane.flow,
                    &mut tool_gate,
                    &completion_tx,
                    &barge_in,
                    &event_tx,
                )
                .await;
            }
            ControlEvent::ToolCompleted { call_id, name, ok } => {
                // A background tool finished — advance the governed flow through
                // the same gate as inline tools, deduped by call_id (#7).
                tool_gate.observe_completion(&call_id, &name, ok, &control_plane.flow, &state);
            }
            ControlEvent::ToolCallCancelled(ids) => {
                // Cancel background tasks first
                if let Some(ref tracker) = background_tracker {
                    tracker.cancel(&ids);
                }
                if let Some(ref disp) = dispatcher {
                    disp.cancel_by_ids(&ids).await;
                }
                let _ = event_tx.send(LiveEvent::ToolCancelled { ids: ids.clone() });
                if let Some(cb) = &callbacks.on_tool_cancelled {
                    dispatch_callback!(callbacks.on_tool_cancelled_mode, cb(ids));
                }
            }
            ControlEvent::Interrupted => {
                // Truncate current model turn on interruption (no mutex)
                transcript_buffer.truncate_current_model_turn();
                if let Some(cb) = &callbacks.on_interrupted {
                    dispatch_callback!(callbacks.on_interrupted_mode, cb());
                }
                // Resume audio forwarding after interrupt callback completes
                shared.interrupted.store(false, Ordering::Release);
                // Re-arm the barge-in token for the next turn: the router
                // cancelled the previous one the moment the interruption
                // arrived (so an in-flight inline tool could be raced).
                *shared.barge_in.lock() = tokio_util::sync::CancellationToken::new();
                let _ = event_tx.send(LiveEvent::Interrupted);
            }
            ControlEvent::TurnComplete => {
                // Reset soft turn detector -- model responded
                if let Some(ref mut std) = control_plane.soft_turn {
                    std.on_model_response();
                }
                // Synthesize the `is_final = true` transcript finalization for this
                // turn. The fast lane only ever emits per-chunk callbacks with
                // `is_final = false`; here we deliver the full accumulated text once
                // with `is_final = true` at the turn boundary. These are sync
                // `Fn(&str, bool)` closures -- call them directly. Fire BEFORE
                // `handle_turn_complete` so finalization precedes `on_turn_complete`,
                // mirroring the partial -> final ASR ordering.
                if !accumulated_output.is_empty()
                    && let Some(cb) = &callbacks.on_output_transcript
                {
                    cb(&accumulated_output, true);
                }
                if !accumulated_input.is_empty()
                    && let Some(cb) = &callbacks.on_input_transcript
                {
                    cb(&accumulated_input, true);
                }
                accumulated_input.clear();
                accumulated_output.clear();
                handle_turn_complete(
                    &callbacks,
                    &writer,
                    &shared,
                    &extractors,
                    &state,
                    &computed,
                    &phase_machine,
                    &watchers,
                    &temporal,
                    &mut transcript_buffer,
                    &mut extraction_turn_tracker,
                    &mut control_plane,
                    &event_tx,
                )
                .await;
                let _ = event_tx.send(LiveEvent::TurnComplete);
            }
            ControlEvent::GoAway(time_left) => {
                let duration = time_left.unwrap_or(Duration::from_secs(60));
                if let Some(cb) = &callbacks.on_go_away {
                    dispatch_callback!(callbacks.on_go_away_mode, cb(duration));
                }
                let _ = event_tx.send(LiveEvent::GoAway {
                    time_left: duration,
                });
            }
            ControlEvent::Connected => {
                if let Some(cb) = &callbacks.on_connected {
                    dispatch_callback!(callbacks.on_connected_mode, cb(writer.clone()));
                }
                let _ = event_tx.send(LiveEvent::Connected);
            }
            ControlEvent::Disconnected(reason) => {
                let _ = event_tx.send(LiveEvent::Disconnected {
                    reason: reason.clone(),
                });
                // Teardown first, and always awaited: these flush durable state
                // (memory reconciliation, for one), so the application's own
                // handler should observe a settled world rather than race it.
                for hook in &callbacks.on_teardown_concurrent {
                    tokio::spawn(hook());
                }
                for hook in &callbacks.on_teardown {
                    hook().await;
                }
                if let Some(cb) = &callbacks.on_disconnected {
                    dispatch_callback!(callbacks.on_disconnected_mode, cb(reason));
                }
            }
            ControlEvent::SessionResumeUpdate(_info) => {
                // Resume info is already stored in shared state by the router.
                // Fire the user-facing resume callback. No matching LiveEvent
                // variant exists, so none is emitted.
                if let Some(cb) = &callbacks.on_resumed {
                    dispatch_callback!(callbacks.on_resumed_mode, cb());
                }
            }
            ControlEvent::GenerationComplete => {
                // Run OnGenerationComplete extractors with pre-truncation transcript
                let gen_extractors: Vec<Arc<dyn TurnExtractor>> = extractors
                    .iter()
                    .filter(|e| matches!(e.trigger(), ExtractionTrigger::OnGenerationComplete))
                    .cloned()
                    .collect();
                if !gen_extractors.is_empty() {
                    // Use snapshot_window_with_current to capture model output before truncation
                    run_extractors_with_window(
                        &gen_extractors,
                        &mut transcript_buffer,
                        &state,
                        &callbacks,
                        true, // include current (pre-finalized) turn
                        &event_tx,
                    )
                    .await;
                }
                // Fire the user-facing generation-complete callback AFTER the
                // OnGenerationComplete extractors have run against the
                // pre-truncation transcript.
                if let Some(cb) = &callbacks.on_generation_complete {
                    dispatch_callback!(callbacks.on_generation_complete_mode, cb());
                }
            }
            ControlEvent::Error(err) => {
                let _ = event_tx.send(LiveEvent::Error(err.clone()));
                if let Some(cb) = &callbacks.on_error {
                    dispatch_callback!(callbacks.on_error_mode, cb(err));
                }
            }
        }
    }

    // Lane exit (event channel closed): graceful drain. Flush any deferred
    // context still queued and run a final persistence snapshot synchronously
    // — the per-turn save is spawn-and-forget and can lose the last turn when
    // the process exits right after disconnect.
    final_drain(
        &writer,
        &shared,
        &state,
        &phase_machine,
        &mut transcript_buffer,
        &control_plane,
    )
    .await;
}
