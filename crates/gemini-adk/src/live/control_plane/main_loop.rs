//! Control lane main loop — dispatches ControlEvents to handlers.

use std::sync::Arc;
use std::time::Duration;

use std::sync::atomic::Ordering;

use gemini_live::session::SessionWriter;

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
use super::lifecycle::handle_turn_complete;
use super::tool_handler::handle_tool_calls;

/// Control lane processor -- handles lifecycle events, tool dispatch,
/// transcript accumulation, extractors, phases, watchers.
///
/// TranscriptBuffer is owned exclusively -- no Arc<Mutex<>> needed.
pub(in crate::live) async fn run_control_lane(
    mut rx: tokio::sync::mpsc::Receiver<ControlEvent>,
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

    // Track which turn each interval-based extractor last ran on.
    let mut extraction_turn_tracker: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();

    while let Some(event) = rx.recv().await {
        match event {
            // -- Transcript accumulation (exclusive to control lane) --
            ControlEvent::InputTranscript(text) => {
                transcript_buffer.push_input(&text);
            }
            ControlEvent::OutputTranscript(text) => {
                transcript_buffer.push_output(&text);
            }

            ControlEvent::ToolCall(calls) => {
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
                    &event_tx,
                )
                .await;
            }
            ControlEvent::ToolCallCancelled(ids) => {
                // Cancel background tasks first
                if let Some(ref tracker) = background_tracker {
                    tracker.cancel(&ids);
                }
                if let Some(ref disp) = dispatcher {
                    disp.cancel_by_ids(&ids).await;
                }
                if let Some(cb) = &callbacks.on_tool_cancelled {
                    dispatch_callback!(callbacks.on_tool_cancelled_mode, cb(ids));
                }
            }
            ControlEvent::Interrupted => {
                // Truncate current model turn on interruption (no mutex)
                transcript_buffer.truncate_current_model_turn();
                if let Some(cb) = &callbacks.on_interrupted {
                    cb().await;
                }
                // Resume audio forwarding after interrupt callback completes
                shared.interrupted.store(false, Ordering::Release);
                let _ = event_tx.send(LiveEvent::Interrupted);
            }
            ControlEvent::TurnComplete => {
                // Reset soft turn detector -- model responded
                if let Some(ref mut std) = control_plane.soft_turn {
                    std.on_model_response();
                }
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
                let duration = time_left
                    .as_deref()
                    .and_then(|s| s.trim_end_matches('s').parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(Duration::from_secs(60));
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
                if let Some(cb) = &callbacks.on_disconnected {
                    dispatch_callback!(callbacks.on_disconnected_mode, cb(reason));
                }
            }
            ControlEvent::SessionResumeUpdate(_info) => {
                // Already stored in shared state by the router
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
            }
            ControlEvent::Error(err) => {
                let _ = event_tx.send(LiveEvent::Error(err.clone()));
                if let Some(cb) = &callbacks.on_error {
                    dispatch_callback!(callbacks.on_error_mode, cb(err));
                }
            }
        }
    }
}
