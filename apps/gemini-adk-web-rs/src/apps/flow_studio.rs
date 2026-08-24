//! Flow Studio — run a JSON-authored, flow-governed session.
//!
//! The browser's Flow Studio editor (`/flows`) composes a
//! [`FlowAppSpec`] — a governed
//! flow DAG plus instruction, greeting, and declarative mock tools — and sends
//! it in the `config` field of the Start message. This app compiles the flow,
//! wires the mock tools onto a shared [`State`], governs the Live session with
//! the flow, and pushes a [`ServerMessage::FlowStatus`] snapshot after every
//! turn and tool call so the editor can light up the DAG live.

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc};
use tracing::info;

use gemini_adk_fluent_rs::live::LiveEvent;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_server_rs::flow_app::{FlowAppSpec, FlowModality};

use crate::app::{AppError, ClientMessage, DemoApp, ServerMessage, WsSender};
use crate::bridge::SessionBridge;
use crate::demo_meta;

/// Runs flow applications authored as JSON in the Flow Studio editor.
pub struct FlowStudio;

/// Snapshot the governed flow's status into a `FlowStatus` message.
fn send_flow_status(tx: &WsSender, handle: &LiveHandle) {
    let Some(explanation) = handle.why_blocked() else {
        return;
    };
    let state = handle.state();
    let done: Vec<String> = state.get("flow:done").unwrap_or_default();
    let mut status = serde_json::to_value(&explanation).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = status.as_object_mut() {
        obj.insert("done".into(), serde_json::json!(done));
        obj.insert(
            "complete".into(),
            serde_json::json!(explanation.missing_requirements.is_empty()),
        );
    }
    let _ = tx.send(ServerMessage::FlowStatus { status });
}

#[async_trait]
impl DemoApp for FlowStudio {
    demo_meta! {
        name: "flow-studio",
        description: "Runs flow applications authored as JSON in the drag-and-drop Flow Studio",
        category: Showcase,
        features: ["flow", "tools", "text"],
        tips: [
            "Author the flow in the Studio editor at /flows, then press Run",
            "Watch steps light up as their completion guards latch",
            "Blocked tools are denied live — ask for a gated action to see enforcement",
        ],
        try_saying: [
            "Let's get started",
        ],
    }

    async fn handle_session(
        &self,
        tx: WsSender,
        mut rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError> {
        info!("FlowStudio session starting");
        let bridge = SessionBridge::new(tx.clone());
        let status_tx = tx.clone();
        bridge
            .run_with(
                self,
                &mut rx,
                |live, start| {
                    let config = start.config.clone().ok_or_else(|| {
                        AppError::Session(
                            "flow-studio requires a flow app spec in Start.config".into(),
                        )
                    })?;
                    let spec = FlowAppSpec::from_value(config).map_err(AppError::Session)?;

                    let validation = spec.validate();
                    if !validation.valid {
                        return Err(AppError::Session(format!(
                            "flow failed to compile: {}",
                            validation.errors.join("; ")
                        )));
                    }

                    let state = State::new();
                    let dispatcher = spec.build_dispatcher(&state);

                    let mut live = live
                        .model(super::live_model())
                        .with_state(state)
                        .instruction(if spec.instruction.is_empty() {
                            "Follow the conversation flow you are given.".to_string()
                        } else {
                            spec.instruction.clone()
                        })
                        .govern(spec.flow.clone());

                    if !spec.tools.is_empty() {
                        live = live.tools(dispatcher);
                    }
                    if let Some(greeting) = &spec.greeting {
                        live = live.greeting(greeting.clone());
                    }
                    live = match spec.modality {
                        FlowModality::Text => live.text_only(),
                        FlowModality::Audio => live.voice(super::resolve_voice(
                            spec.voice.as_deref().or(start.voice.as_deref()),
                        )),
                    };
                    Ok(live)
                },
                move |handle| {
                    // Push an initial snapshot, then one after every turn
                    // boundary and tool execution, so the editor's DAG tracks
                    // the live marking.
                    send_flow_status(&status_tx, handle);
                    let mut events = handle.events();
                    let handle = handle.clone();
                    tokio::spawn(async move {
                        loop {
                            match events.recv().await {
                                Ok(LiveEvent::TurnComplete)
                                | Ok(LiveEvent::ToolExecution { .. })
                                | Ok(LiveEvent::Extraction { .. }) => {
                                    send_flow_status(&status_tx, &handle);
                                }
                                Err(broadcast::error::RecvError::Closed) => break,
                                _ => {}
                            }
                        }
                    });
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use gemini_adk_server_rs::flow_app::FlowAppSpec;

    fn assert_example_valid(name: &str, json: &str) {
        let value: serde_json::Value = serde_json::from_str(json).expect("well-formed JSON");
        let spec = FlowAppSpec::from_value(value).expect("parses as FlowAppSpec");
        let validation = spec.validate();
        assert!(
            validation.valid,
            "example '{name}' failed to compile: {:?}",
            validation.errors
        );
    }

    #[test]
    fn bundled_examples_compile() {
        assert_example_valid(
            "collections",
            include_str!("../../static/examples/flows/collections.json"),
        );
        assert_example_valid(
            "restaurant",
            include_str!("../../static/examples/flows/restaurant.json"),
        );
    }
}
