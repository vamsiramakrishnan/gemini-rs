//! Server message handling — decode and dispatch session events.

use std::sync::Arc;

use base64::Engine;
use tokio::sync::broadcast;

use crate::protocol::messages::*;
use crate::protocol::types::*;
use crate::session::{ResumeInfo, SessionEvent, SessionPhase, SessionState};

/// Action to take after processing a server message.
pub(super) enum MessageAction {
    Continue,
    GoAway(Option<String>),
}

/// Process a decoded [`ServerMessage`] and emit appropriate session events.
///
/// This is the shared message handler used by the generic transport path.
pub(super) fn handle_server_msg(
    msg: ServerMessage,
    state: &Arc<SessionState>,
    event_tx: &broadcast::Sender<SessionEvent>,
) -> MessageAction {
    match msg {
        ServerMessage::ServerContent(sc) => {
            let content = sc.server_content;

            // Handle interruption
            if content.interrupted.unwrap_or(false) {
                state.interrupt_turn();
                let _ = state.transition_to(SessionPhase::Interrupted);
                let _ = event_tx.send(SessionEvent::Interrupted);
                let _ = state.transition_to(SessionPhase::Active);
                return MessageAction::Continue;
            }

            // Handle model turn content
            if let Some(model_turn) = content.model_turn {
                // Ensure we're in ModelSpeaking
                if state.phase() == SessionPhase::Active {
                    let _ = state.transition_to(SessionPhase::ModelSpeaking);
                    state.start_turn();
                }

                for part in &model_turn.parts {
                    match part {
                        Part::Text { text } => {
                            state.append_text(text);
                            let _ = event_tx.send(SessionEvent::TextDelta(text.clone()));
                        }
                        Part::InlineData { inline_data } => {
                            state.mark_audio();
                            if let Ok(audio_bytes) =
                                base64::engine::general_purpose::STANDARD
                                    .decode(&inline_data.data)
                            {
                                let _ =
                                    event_tx.send(SessionEvent::AudioData(bytes::Bytes::from(audio_bytes)));
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Handle input transcription
            if let Some(transcription) = content.input_transcription {
                if let Some(text) = transcription.text {
                    let _ = event_tx.send(SessionEvent::InputTranscription(text));
                }
            }

            // Handle output transcription
            if let Some(transcription) = content.output_transcription {
                if let Some(text) = transcription.text {
                    let _ = event_tx.send(SessionEvent::OutputTranscription(text));
                }
            }

            // Handle usage metadata (present on most server content messages)
            if let Some(usage) = sc.usage_metadata {
                let _ = event_tx.send(SessionEvent::Usage(usage));
            }

            // Handle generation complete (fires before turn_complete)
            if content.generation_complete.unwrap_or(false) {
                let _ = event_tx.send(SessionEvent::GenerationComplete);
            }

            // Handle turn complete
            if content.turn_complete.unwrap_or(false) {
                if let Some(turn) = state.complete_turn() {
                    if !turn.text.is_empty() {
                        let _ = event_tx.send(SessionEvent::TextComplete(turn.text));
                    }
                }
                let _ = event_tx.send(SessionEvent::TurnComplete);
                let _ = state.transition_to(SessionPhase::Active);
            }
        }

        ServerMessage::ToolCall(tc) => {
            let calls = tc.tool_call.function_calls;
            // Track tool calls in turn
            if let Some(turn) = state.current_turn.lock().as_mut() {
                turn.tool_calls.extend(calls.clone());
            }
            let _ = state.transition_to(SessionPhase::ToolCallPending);
            let _ = event_tx.send(SessionEvent::ToolCall(calls));
        }

        ServerMessage::ToolCallCancellation(tc) => {
            let ids = tc.tool_call_cancellation.ids;
            let _ = event_tx.send(SessionEvent::ToolCallCancelled(ids));
        }

        ServerMessage::GoAway(ga) => {
            return MessageAction::GoAway(ga.go_away.time_left);
        }

        ServerMessage::SetupComplete(_) => {
            // Should not happen after initial setup, but handle gracefully
        }

        ServerMessage::SessionResumptionUpdate(sru) => {
            let payload = sru.session_resumption_update;
            if let Some(ref handle) = payload.new_handle {
                *state.resume_handle.lock() = Some(handle.clone());
                let _ = event_tx.send(SessionEvent::SessionResumeUpdate(ResumeInfo {
                    handle: handle.clone(),
                    resumable: payload.resumable.unwrap_or(true),
                    last_consumed_index: payload.last_consumed_client_message_index,
                }));
            }
        }

        ServerMessage::VoiceActivity(msg) => {
            if let Some(vat) = msg.voice_activity.voice_activity_type {
                match vat {
                    VoiceActivityType::VoiceActivityStart => {
                        let _ = event_tx.send(SessionEvent::VoiceActivityStart);
                    }
                    VoiceActivityType::VoiceActivityEnd => {
                        let _ = event_tx.send(SessionEvent::VoiceActivityEnd);
                    }
                }
            }
        }

        ServerMessage::Unknown(_) => {
            // Forward-compatible: ignore unknown messages
        }
    }

    MessageAction::Continue
}
