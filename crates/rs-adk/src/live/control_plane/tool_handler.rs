//! Tool call handling — phase filtering, dispatch, background tools.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use rs_genai::prelude::FunctionResponse;
use rs_genai::session::SessionWriter;

use crate::state::State;
use crate::tool::ToolDispatcher;

use crate::live::background_tool::BackgroundToolTracker;
use crate::live::callbacks::EventCallbacks;
use crate::live::extractor::{ExtractionTrigger, TurnExtractor};
use crate::live::phase::PhaseMachine;
use crate::live::transcript::TranscriptBuffer;

use super::extractors::run_extractors;

/// Handle tool calls: phase filtering -> user callback -> auto-dispatch -> interceptor -> send.
pub(in crate::live) async fn handle_tool_calls(
    calls: Vec<rs_genai::prelude::FunctionCall>,
    callbacks: &EventCallbacks,
    dispatcher: &Option<Arc<ToolDispatcher>>,
    writer: &Arc<dyn SessionWriter>,
    state: &State,
    phase_machine: &Option<tokio::sync::Mutex<PhaseMachine>>,
    transcript_buffer: &mut TranscriptBuffer,
    execution_modes: &std::collections::HashMap<
        String,
        crate::live::background_tool::ToolExecutionMode,
    >,
    background_tracker: &Option<Arc<BackgroundToolTracker>>,
    extractors: &[Arc<dyn TurnExtractor>],
) {
    // 0. Phase-scoped tool filtering: reject calls not in phase's allowed list
    let (allowed_calls, rejected_responses) = if let Some(ref pm) = phase_machine {
        let active_tools = {
            let pm_guard = pm.lock().await;
            pm_guard.active_tools().map(|t| t.to_vec())
        };
        if let Some(active_tools) = active_tools {
            let mut allowed = Vec::new();
            let mut rejected = Vec::new();
            for call in calls {
                if active_tools.iter().any(|t| t == &call.name) {
                    allowed.push(call);
                } else {
                    rejected.push(FunctionResponse {
                        name: call.name.clone(),
                        response: serde_json::json!({
                            "error": format!(
                                "Tool '{}' is not available in the current conversation phase.",
                                call.name
                            )
                        }),
                        id: call.id.clone(),
                        scheduling: None,
                    });
                }
            }
            (allowed, rejected)
        } else {
            (calls, Vec::new())
        }
    } else {
        (calls, Vec::new())
    };

    // 1. Check user callback for override (receives State)
    let responses = if allowed_calls.is_empty() && !rejected_responses.is_empty() {
        Some(rejected_responses.clone())
    } else if let Some(cb) = &callbacks.on_tool_call {
        let mut result = cb(allowed_calls.clone(), state.clone()).await;
        if !rejected_responses.is_empty() {
            let r = result.get_or_insert_with(Vec::new);
            r.extend(rejected_responses.clone());
        }
        result
    } else {
        None
    };

    // 2. If no override, auto-dispatch via ToolDispatcher (split standard vs background)
    let (responses, background_spawns) = match responses {
        Some(r) => (r, Vec::new()),
        None => {
            let mut results: Vec<FunctionResponse> = rejected_responses;
            let mut bg_spawns: Vec<(
                rs_genai::prelude::FunctionCall,
                Option<Arc<dyn crate::live::background_tool::ResultFormatter>>,
            )> = Vec::new();

            if let Some(ref disp) = dispatcher {
                for call in &allowed_calls {
                    let mode = execution_modes.get(&call.name);
                    match mode {
                        Some(crate::live::background_tool::ToolExecutionMode::Background {
                            formatter,
                            scheduling,
                        }) => {
                            // Send immediate ack
                            let fmt: &dyn crate::live::background_tool::ResultFormatter = formatter
                                .as_ref()
                                .map(|f| f.as_ref())
                                .unwrap_or(&crate::live::background_tool::DefaultResultFormatter);
                            let ack = fmt.format_running(call);
                            results.push(FunctionResponse {
                                name: call.name.clone(),
                                response: ack,
                                id: call.id.clone(),
                                scheduling: *scheduling,
                            });
                            bg_spawns.push((call.clone(), formatter.clone()));
                        }
                        _ => {
                            // Standard: execute inline
                            match disp.call_function(&call.name, call.args.clone()).await {
                                Ok(result) => results.push(FunctionResponse {
                                    name: call.name.clone(),
                                    response: result,
                                    id: call.id.clone(),
                                    scheduling: None,
                                }),
                                Err(e) => results.push(FunctionResponse {
                                    name: call.name.clone(),
                                    response: serde_json::json!({"error": e.to_string()}),
                                    id: call.id.clone(),
                                    scheduling: None,
                                }),
                            }
                        }
                    }
                }
            } else if results.is_empty() {
                #[cfg(feature = "tracing-support")]
                tracing::warn!("Tool call received but no dispatcher or callback registered");
            }
            (results, bg_spawns)
        }
    };

    // 3. Run through before_tool_response interceptor
    let responses = if let Some(cb) = &callbacks.before_tool_response {
        cb(responses, state.clone()).await
    } else {
        responses
    };

    // 4. Record tool call summaries in transcript buffer (no mutex)
    for resp in &responses {
        let args = allowed_calls
            .iter()
            .find(|c| c.name == resp.name)
            .map(|c| &c.args)
            .unwrap_or(&serde_json::Value::Null);
        transcript_buffer.push_tool_call(resp.name.clone(), args, &resp.response);
    }

    // 5. Send tool responses (standard + ack) back to Gemini
    if !responses.is_empty() {
        if let Err(_e) = writer.send_tool_response(responses).await {
            #[cfg(feature = "tracing-support")]
            tracing::error!("Failed to send tool response: {_e}");
        }
    }

    // 6. Spawn background tool tasks
    for (call, formatter) in background_spawns {
        let disp = dispatcher.clone();
        let bg_writer = writer.clone();
        let tracker = background_tracker.clone();
        let call_id = call.id.clone().unwrap_or_default();
        let cancel = CancellationToken::new();

        let handle = tokio::spawn(async move {
            let result = if let Some(ref d) = disp {
                d.call_function(&call.name, call.args.clone())
                    .await
                    .map_err(|e| crate::error::ToolError::ExecutionFailed(e.to_string()))
            } else {
                Err(crate::error::ToolError::NotFound(call.name.clone()))
            };

            let fmt: &dyn crate::live::background_tool::ResultFormatter = formatter
                .as_ref()
                .map(|f| f.as_ref())
                .unwrap_or(&crate::live::background_tool::DefaultResultFormatter);
            let formatted = fmt.format_result(&call, result);

            bg_writer
                .send_tool_response(vec![FunctionResponse {
                    name: call.name.clone(),
                    response: formatted,
                    id: call.id.clone(),
                    scheduling: None,
                }])
                .await
                .ok();

            // Self-cleanup from tracker
            if let Some(ref t) = tracker {
                t.remove(&call.id.clone().unwrap_or_default());
            }
        });

        // Register in tracker for cancellation
        if let Some(ref t) = background_tracker {
            t.spawn(call_id, handle, cancel);
        }
    }

    // 7. Run AfterToolCall extractors
    let after_tool_extractors: Vec<Arc<dyn TurnExtractor>> = extractors
        .iter()
        .filter(|e| matches!(e.trigger(), ExtractionTrigger::AfterToolCall))
        .cloned()
        .collect();
    run_extractors(&after_tool_extractors, transcript_buffer, state, callbacks).await;
}
