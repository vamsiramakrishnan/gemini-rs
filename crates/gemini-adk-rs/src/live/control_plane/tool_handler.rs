//! Tool call handling — phase filtering, dispatch, background tools.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use gemini_genai_rs::prelude::FunctionResponse;
use gemini_genai_rs::session::SessionWriter;

use crate::state::State;
use crate::tool::ToolDispatcher;

use crate::live::background_tool::BackgroundToolTracker;
use crate::live::callbacks::EventCallbacks;
use crate::live::events::LiveEvent;
use crate::live::extractor::{ExtractionTrigger, TurnExtractor};
use crate::live::phase::PhaseMachine;
use crate::live::transcript::TranscriptBuffer;

use super::extractors::run_extractors;
use super::tool_gate::ToolGate;

/// Handle tool calls: phase filtering -> user callback -> auto-dispatch -> interceptor -> send.
///
/// # Barge-in cancellation
///
/// Inline (standard-mode) dispatch races each tool future against the
/// `barge_in` token, which the router cancels the moment an `Interrupted`
/// event arrives. On cancellation the tool future is **dropped at its current
/// await point** — tools must therefore be drop-safe (no critical sections
/// that corrupt state when abandoned mid-await). A cancelled call:
///
/// - sends **no** `FunctionResponse` back to the model (matching the wire
///   contract for cancelled calls),
/// - does **not** advance the governed-flow [`ToolGate`],
/// - is surfaced as [`LiveEvent::ToolCancelled`].
///
/// Remaining inline calls in the same batch short-circuit to cancelled as
/// well (the token stays cancelled until the control lane processes the
/// interruption and re-arms it).
#[allow(
    clippy::too_many_arguments,
    reason = "tool-lifecycle stage entry point: each parameter is one control-plane subsystem (dispatcher, gate, middleware, transcript)"
)]
pub(in crate::live) async fn handle_tool_calls(
    calls: Vec<gemini_genai_rs::prelude::FunctionCall>,
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
    middleware: &Arc<crate::middleware::MiddlewareChain>,
    flow: &Option<crate::flow::SharedFlowMonitor>,
    tool_gate: &mut ToolGate,
    completion_tx: &tokio::sync::mpsc::WeakSender<crate::live::processor::ControlEvent>,
    barge_in: &CancellationToken,
    event_tx: &tokio::sync::broadcast::Sender<LiveEvent>,
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

    // Inline calls cancelled by a user barge-in (no response is sent for them).
    let mut cancelled_calls: Vec<String> = Vec::new();

    // 2. If no override, auto-dispatch via ToolDispatcher (split standard vs background)
    let (responses, background_spawns) = match responses {
        Some(r) => (r, Vec::new()),
        None => {
            let mut results: Vec<FunctionResponse> = rejected_responses;
            let mut bg_spawns: Vec<(
                gemini_genai_rs::prelude::FunctionCall,
                Option<Arc<dyn crate::live::background_tool::ResultFormatter>>,
            )> = Vec::new();

            if let Some(ref disp) = dispatcher {
                for call in &allowed_calls {
                    // Flow governance gate: deny inadmissible tools (out-of-order,
                    // once-violated, gated) in Enforce mode; record in Observe.
                    // Lock scope is the synchronous admissibility check only.
                    if let Some(mon) = flow {
                        let denial = {
                            let mon = mon.lock();
                            match mon.admits_tool(&call.name, state) {
                                Err(reason) if mon.mode() == crate::flow::Enforcement::Enforce => {
                                    Some(reason)
                                }
                                _ => None,
                            }
                        };
                        if let Some(reason) = denial {
                            results.push(FunctionResponse {
                                name: call.name.clone(),
                                response: serde_json::json!({ "error": reason }),
                                id: call.id.clone(),
                                scheduling: None,
                            });
                            continue;
                        }
                    }
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
                            // A background tool is NOT recorded as flow-ok here: its
                            // real outcome (success, `before_tool` veto, or failure) is
                            // only known when the spawned task finishes. The task posts a
                            // `ControlEvent::ToolCompleted` back to the control lane on
                            // completion, which advances the flow through the same gate as
                            // inline tools (#7). A `before_tool` veto posts nothing, so a
                            // vetoed tool never advances the flow — matching the inline
                            // path. `Guard::resolved(..)` / `captured([..])` still work for
                            // gating on the delivered result; `called_ok(..)` now works too.
                        }
                        _ => {
                            // Standard: execute inline, wrapped in middleware hooks.
                            // A `before_tool` error vetoes execution (e.g. guardrails).
                            if let Err(e) = middleware.run_before_tool(call).await {
                                results.push(FunctionResponse {
                                    name: call.name.clone(),
                                    response: serde_json::json!({"error": e.to_string()}),
                                    id: call.id.clone(),
                                    scheduling: None,
                                });
                                continue;
                            }
                            // Race the tool against the barge-in signal so an
                            // interruption is never stuck behind a slow tool.
                            // `biased` makes an already-cancelled token win
                            // without polling the tool future at all.
                            let dispatched = tokio::select! {
                                biased;
                                _ = barge_in.cancelled() => None,
                                result = disp.call_function(&call.name, call.args.clone()) => {
                                    Some(result)
                                }
                            };
                            let Some(call_result) = dispatched else {
                                // Cancelled: the tool future was dropped at its
                                // current await point. No response, no gate
                                // advance — the model's turn was interrupted.
                                cancelled_calls.push(call.id.clone().unwrap_or_default());
                                continue;
                            };
                            match call_result {
                                Ok(result) => {
                                    let _ = middleware.run_after_tool(call, &result).await;
                                    // Inline completion advances the governed flow
                                    // through the single shared gate (#7).
                                    tool_gate.observe_completion(
                                        call.id.as_deref().unwrap_or(""),
                                        &call.name,
                                        true,
                                        flow,
                                        state,
                                    );
                                    results.push(FunctionResponse {
                                        name: call.name.clone(),
                                        response: result,
                                        id: call.id.clone(),
                                        scheduling: None,
                                    });
                                }
                                Err(e) => {
                                    let _ = middleware.run_on_tool_error(call, &e).await;
                                    tool_gate.observe_completion(
                                        call.id.as_deref().unwrap_or(""),
                                        &call.name,
                                        false,
                                        flow,
                                        state,
                                    );
                                    results.push(FunctionResponse {
                                        name: call.name.clone(),
                                        response: serde_json::json!({"error": e.to_string()}),
                                        id: call.id.clone(),
                                        scheduling: None,
                                    });
                                }
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

    // Surface barge-in cancellations. No response is sent for a cancelled
    // call and the governed-flow gate was not advanced for it.
    if !cancelled_calls.is_empty() {
        let _ = event_tx.send(LiveEvent::ToolCancelled {
            ids: cancelled_calls,
        });
    }

    // 3. Run through before_tool_response interceptor
    let responses = if let Some(cb) = &callbacks.before_tool_response {
        cb(responses, state.clone()).await
    } else {
        responses
    };

    // 4. Record tool call summaries in transcript buffer (no mutex) + emit LiveEvents
    for resp in &responses {
        let args = allowed_calls
            .iter()
            .find(|c| c.name == resp.name)
            .map(|c| &c.args)
            .unwrap_or(&serde_json::Value::Null);
        transcript_buffer.push_tool_call(resp.name.clone(), args, &resp.response);
        let _ = event_tx.send(LiveEvent::ToolExecution {
            name: resp.name.clone(),
            args: args.clone(),
            result: resp.response.clone(),
        });
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
        let mw = middleware.clone();
        let call_id = call.id.clone().unwrap_or_default();
        let completion_tx = completion_tx.clone();
        let cancel = CancellationToken::new();

        let handle = tokio::spawn(async move {
            // A `before_tool` veto blocks execution for background tools too,
            // matching the standard path and the `Live::middleware` contract:
            // send an error response, skip dispatch, and self-clean. A vetoed tool
            // posts no completion, so it never advances the governed flow (#7).
            if let Err(veto) = mw.run_before_tool(&call).await {
                bg_writer
                    .send_tool_response(vec![FunctionResponse {
                        name: call.name.clone(),
                        response: serde_json::json!({ "error": veto.to_string() }),
                        id: call.id.clone(),
                        scheduling: None,
                    }])
                    .await
                    .ok();
                if let Some(ref t) = tracker {
                    t.remove(&call.id.clone().unwrap_or_default());
                }
                return;
            }
            let result = if let Some(ref d) = disp {
                d.call_function(&call.name, call.args.clone())
                    .await
                    .map_err(|e| crate::error::ToolError::ExecutionFailed(e.to_string()))
            } else {
                Err(crate::error::ToolError::NotFound(call.name.clone()))
            };
            match &result {
                Ok(value) => {
                    let _ = mw.run_after_tool(&call, value).await;
                }
                Err(e) => {
                    let _ = mw.run_on_tool_error(&call, e).await;
                }
            }

            // Post the completion back to the control lane so it advances the
            // governed flow through the same gate as inline tools (#7). Best
            // effort: if the lane has shut down, the upgrade/send simply fails.
            if let Some(tx) = completion_tx.upgrade() {
                let _ = tx
                    .send(crate::live::processor::ControlEvent::ToolCompleted {
                        call_id: call.id.clone().unwrap_or_default(),
                        name: call.name.clone(),
                        ok: result.is_ok(),
                    })
                    .await;
            }

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
    run_extractors(
        &after_tool_extractors,
        transcript_buffer,
        state,
        callbacks,
        event_tx,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use gemini_genai_rs::prelude::{Content, FunctionCall};
    use gemini_genai_rs::session::SessionError;
    use serde_json::json;

    use crate::flow::{Enforcement, Flow, FlowMonitor, Guard};
    use crate::live::background_tool::ToolExecutionMode;
    use crate::live::processor::ControlEvent;
    use crate::middleware::{Middleware, MiddlewareChain};
    use crate::tool::{SimpleTool, ToolDispatcher};

    /// Middleware that counts hook invocations and can veto `before_tool`.
    #[derive(Default)]
    struct CountingMiddleware {
        before: AtomicUsize,
        after: AtomicUsize,
        errors: AtomicUsize,
        veto: bool,
    }

    #[async_trait]
    impl Middleware for CountingMiddleware {
        fn name(&self) -> &str {
            "counting"
        }
        async fn before_tool(&self, _call: &FunctionCall) -> Result<(), crate::error::AgentError> {
            self.before.fetch_add(1, Ordering::SeqCst);
            if self.veto {
                Err(crate::error::AgentError::Other("vetoed".into()))
            } else {
                Ok(())
            }
        }
        async fn after_tool(
            &self,
            _call: &FunctionCall,
            _result: &serde_json::Value,
        ) -> Result<(), crate::error::AgentError> {
            self.after.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn on_tool_error(
            &self,
            _call: &FunctionCall,
            _err: &crate::error::ToolError,
        ) -> Result<(), crate::error::AgentError> {
            self.errors.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct NoopWriter;
    #[async_trait]
    impl SessionWriter for NoopWriter {
        async fn send_audio(&self, _: Vec<u8>) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_text(&self, _: String) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_tool_response(&self, _: Vec<FunctionResponse>) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_client_content(&self, _: Vec<Content>, _: bool) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_video(&self, _: Vec<u8>) -> Result<(), SessionError> {
            Ok(())
        }
        async fn update_instruction(&self, _: String) -> Result<(), SessionError> {
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

    fn dispatcher_with_counter(counter: Arc<AtomicUsize>) -> Arc<ToolDispatcher> {
        let mut d = ToolDispatcher::new();
        d.register(SimpleTool::new("echo", "echoes", None, move |args| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(json!({ "echoed": args }))
            }
        }));
        Arc::new(d)
    }

    async fn run_one(middleware: Arc<MiddlewareChain>, tool_runs: Arc<AtomicUsize>) {
        let dispatcher = Some(dispatcher_with_counter(tool_runs));
        let writer: Arc<dyn SessionWriter> = Arc::new(NoopWriter);
        let callbacks = EventCallbacks::default();
        let state = State::new();
        let mut transcript = TranscriptBuffer::new();
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let (ctrl_tx, _ctrl_rx) =
            tokio::sync::mpsc::channel::<crate::live::processor::ControlEvent>(8);
        let call = FunctionCall {
            name: "echo".into(),
            args: json!({ "x": 1 }),
            id: Some("c1".into()),
        };
        handle_tool_calls(
            vec![call],
            &callbacks,
            &dispatcher,
            &writer,
            &state,
            &None,
            &mut transcript,
            &std::collections::HashMap::new(),
            &None,
            &[],
            &middleware,
            &None,
            &mut ToolGate::new(),
            &ctrl_tx.downgrade(),
            &CancellationToken::new(),
            &tx,
        )
        .await;
    }

    #[tokio::test]
    async fn middleware_fires_before_and_after_tool() {
        let mw = Arc::new(CountingMiddleware::default());
        let mut chain = MiddlewareChain::new();
        chain.add(mw.clone());
        let tool_runs = Arc::new(AtomicUsize::new(0));

        run_one(Arc::new(chain), tool_runs.clone()).await;

        assert_eq!(
            mw.before.load(Ordering::SeqCst),
            1,
            "before_tool should fire"
        );
        assert_eq!(mw.after.load(Ordering::SeqCst), 1, "after_tool should fire");
        assert_eq!(mw.errors.load(Ordering::SeqCst), 0);
        assert_eq!(tool_runs.load(Ordering::SeqCst), 1, "tool should execute");
    }

    #[tokio::test]
    async fn before_tool_error_vetoes_execution() {
        let mw = Arc::new(CountingMiddleware {
            veto: true,
            ..Default::default()
        });
        let mut chain = MiddlewareChain::new();
        chain.add(mw.clone());
        let tool_runs = Arc::new(AtomicUsize::new(0));

        run_one(Arc::new(chain), tool_runs.clone()).await;

        assert_eq!(mw.before.load(Ordering::SeqCst), 1);
        assert_eq!(
            mw.after.load(Ordering::SeqCst),
            0,
            "after must not fire on veto"
        );
        assert_eq!(
            tool_runs.load(Ordering::SeqCst),
            0,
            "tool must not run when before_tool vetoes"
        );
    }

    /// A writer that records tool-response batches (for barge-in assertions).
    #[derive(Default)]
    struct RecordingToolWriter {
        sent: parking_lot::Mutex<Vec<Vec<FunctionResponse>>>,
    }
    #[async_trait]
    impl SessionWriter for RecordingToolWriter {
        async fn send_audio(&self, _: Vec<u8>) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_text(&self, _: String) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_tool_response(
            &self,
            responses: Vec<FunctionResponse>,
        ) -> Result<(), SessionError> {
            self.sent.lock().push(responses);
            Ok(())
        }
        async fn send_client_content(&self, _: Vec<Content>, _: bool) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_video(&self, _: Vec<u8>) -> Result<(), SessionError> {
            Ok(())
        }
        async fn update_instruction(&self, _: String) -> Result<(), SessionError> {
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

    #[tokio::test]
    async fn barge_in_cancels_slow_inline_tool_without_response_or_flow_advance() {
        // A slow inline tool that would block the control lane for 10s.
        let completed = Arc::new(AtomicUsize::new(0));
        let completed_clone = completed.clone();
        let mut d = ToolDispatcher::new();
        d.register(SimpleTool::new("slow", "slow tool", None, move |_args| {
            let completed = completed_clone.clone();
            async move {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                completed.fetch_add(1, Ordering::SeqCst);
                Ok(json!({ "done": true }))
            }
        }));
        let dispatcher = Some(Arc::new(d));

        let rec = Arc::new(RecordingToolWriter::default());
        let writer: Arc<dyn SessionWriter> = rec.clone();
        let callbacks = EventCallbacks::default();
        let state = State::new();
        let mut transcript = TranscriptBuffer::new();
        let (tx, mut live_rx) = tokio::sync::broadcast::channel(16);
        let (ctrl_tx, _ctrl_rx) = tokio::sync::mpsc::channel::<ControlEvent>(8);
        let middleware = Arc::new(MiddlewareChain::new());

        // Governed flow: "run" completes on called_ok("slow"). A cancelled
        // call must NOT advance it.
        let flow_def = Flow::new()
            .step("run")
            .done(Guard::called_ok("slow"))
            .step("end")
            .after("run")
            .terminal()
            .build()
            .expect("valid flow");
        let flow = Some(FlowMonitor::new(flow_def, Enforcement::Observe).into_shared());
        let mut gate = ToolGate::new();

        // Barge-in fires 50ms into the 10s tool.
        let barge_in = CancellationToken::new();
        let trigger = barge_in.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            trigger.cancel();
        });

        let call = FunctionCall {
            name: "slow".into(),
            args: json!({}),
            id: Some("c1".into()),
        };
        let ran = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            handle_tool_calls(
                vec![call],
                &callbacks,
                &dispatcher,
                &writer,
                &state,
                &None,
                &mut transcript,
                &std::collections::HashMap::new(),
                &None,
                &[],
                &middleware,
                &flow,
                &mut gate,
                &ctrl_tx.downgrade(),
                &barge_in,
                &tx,
            ),
        )
        .await;
        assert!(
            ran.is_ok(),
            "barge-in must win over a slow inline tool (was blocked >2s)"
        );

        // No response was sent for the cancelled call.
        assert!(
            rec.sent.lock().is_empty(),
            "no FunctionResponse may be sent for a cancelled call"
        );
        // The tool never completed (its future was dropped).
        assert_eq!(completed.load(Ordering::SeqCst), 0);

        // The governed flow was not advanced.
        let mon = flow.as_ref().unwrap();
        mon.lock().on_turn(&state);
        assert!(
            !mon.lock().marking().done.contains("run"),
            "a cancelled call must not advance the governed flow"
        );

        // The cancellation is surfaced as a LiveEvent.
        let mut saw_cancelled = false;
        while let Ok(ev) = live_rx.try_recv() {
            if let LiveEvent::ToolCancelled { ids } = ev {
                assert_eq!(ids, vec!["c1".to_string()]);
                saw_cancelled = true;
            }
        }
        assert!(saw_cancelled, "LiveEvent::ToolCancelled must be emitted");
    }

    #[tokio::test]
    async fn pre_cancelled_barge_in_skips_inline_dispatch_entirely() {
        // If the interruption already happened, the biased select must not
        // even poll the tool future.
        let ran = Arc::new(AtomicUsize::new(0));
        let dispatcher = Some(dispatcher_with_counter(ran.clone()));
        let rec = Arc::new(RecordingToolWriter::default());
        let writer: Arc<dyn SessionWriter> = rec.clone();
        let callbacks = EventCallbacks::default();
        let state = State::new();
        let mut transcript = TranscriptBuffer::new();
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let (ctrl_tx, _ctrl_rx) = tokio::sync::mpsc::channel::<ControlEvent>(8);
        let middleware = Arc::new(MiddlewareChain::new());

        let barge_in = CancellationToken::new();
        barge_in.cancel();

        let call = FunctionCall {
            name: "echo".into(),
            args: json!({}),
            id: Some("c1".into()),
        };
        handle_tool_calls(
            vec![call],
            &callbacks,
            &dispatcher,
            &writer,
            &state,
            &None,
            &mut transcript,
            &std::collections::HashMap::new(),
            &None,
            &[],
            &middleware,
            &None,
            &mut ToolGate::new(),
            &ctrl_tx.downgrade(),
            &barge_in,
            &tx,
        )
        .await;

        assert_eq!(ran.load(Ordering::SeqCst), 0, "tool must not run");
        assert!(rec.sent.lock().is_empty(), "no response for cancelled call");
    }

    #[tokio::test]
    async fn background_tool_posts_completion_that_advances_the_flow() {
        // #7 step B: a background tool can't reach the synchronous FlowMonitor,
        // so it posts a ToolCompleted back to the control lane, which routes it
        // through the same gate as inline tools.
        let tool_runs = Arc::new(AtomicUsize::new(0));
        let dispatcher = Some(dispatcher_with_counter(tool_runs.clone()));
        let writer: Arc<dyn SessionWriter> = Arc::new(NoopWriter);
        let callbacks = EventCallbacks::default();
        let state = State::new();
        let mut transcript = TranscriptBuffer::new();
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let (ctrl_tx, mut ctrl_rx) = tokio::sync::mpsc::channel::<ControlEvent>(8);
        let middleware = Arc::new(MiddlewareChain::new());

        let mut modes = std::collections::HashMap::new();
        modes.insert(
            "echo".to_string(),
            ToolExecutionMode::Background {
                formatter: None,
                scheduling: None,
            },
        );

        // A flow whose first step completes on `called_ok("echo")`.
        let flow_def = Flow::new()
            .step("run")
            .done(Guard::called_ok("echo"))
            .step("end")
            .after("run")
            .terminal()
            .build()
            .expect("valid flow");
        let flow = Some(FlowMonitor::new(flow_def, Enforcement::Observe).into_shared());
        let mut gate = ToolGate::new();

        let call = FunctionCall {
            name: "echo".into(),
            args: json!({ "x": 1 }),
            id: Some("c1".into()),
        };
        handle_tool_calls(
            vec![call],
            &callbacks,
            &dispatcher,
            &writer,
            &state,
            &None,
            &mut transcript,
            &modes,
            &None,
            &[],
            &middleware,
            &flow,
            &mut gate,
            &ctrl_tx.downgrade(),
            &CancellationToken::new(),
            &tx,
        )
        .await;

        // The background task runs detached and posts its completion back.
        let evt = ctrl_rx.recv().await.expect("completion posted");
        match evt {
            ControlEvent::ToolCompleted { call_id, name, ok } => {
                assert_eq!(call_id, "c1");
                assert_eq!(name, "echo");
                assert!(ok, "echo succeeded");
                // Routing it through the gate advances the governed flow.
                gate.observe_completion(&call_id, &name, ok, &flow, &state);
            }
            _ => panic!("expected ToolCompleted"),
        }
        assert_eq!(tool_runs.load(Ordering::SeqCst), 1, "background tool ran");
        let mon = flow.as_ref().unwrap();
        mon.lock().on_turn(&state);
        assert!(
            mon.lock().marking().done.contains("run"),
            "background completion latched the step done"
        );
    }
}
