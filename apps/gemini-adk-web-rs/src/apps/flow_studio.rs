//! Flow Studio — run a JSON-authored, spec-driven session.
//!
//! The browser's Flow Studio editor (`/flows`) composes a [`SessionSpec`] —
//! a governed flow DAG plus instruction, greeting, declarative tools
//! (mock/HTTP/MCP), extraction, phases, and watchers — and sends it in the
//! `config` field of the Start message. This app applies the spec to a Live
//! builder ([`SessionSpec::apply`]) and pushes a
//! [`ServerMessage::FlowStatus`] snapshot (with per-step guard truth trees)
//! after every turn and tool call so the editor can light up the DAG live.
//! Posture edits arrive mid-session as `UpdateFlowPostures` and steer the
//! next turn.

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc};
use tracing::info;

use gemini_adk_fluent_rs::live::LiveEvent;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::spec::{SessionSpec, SpecResources};

use crate::app::{AppError, ClientMessage, DemoApp, ServerMessage, WsSender};
use crate::bridge::SessionBridge;
use crate::demo_meta;

/// Runs session specs authored as JSON in the Flow Studio editor.
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
        description: "Runs session specs authored as JSON in the drag-and-drop Flow Studio",
        category: Showcase,
        features: ["flow", "tools", "text"],
        tips: [
            "Author the spec in the Studio editor at /flows, then press Run",
            "Watch steps light up as their completion guards latch — hover for the per-atom truth tree",
            "Edit a posture while connected: the change steers the very next turn",
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
                            "flow-studio requires a session spec in Start.config".into(),
                        )
                    })?;
                    let spec = SessionSpec::from_value(config).map_err(AppError::Session)?;

                    let resources = SpecResources {
                        extraction_llm: (!spec.extract.is_empty())
                            .then(super::build_extraction_llm),
                    };
                    let state = State::new();
                    spec.apply(live.model(super::live_model()), &state, &resources)
                        .map_err(AppError::Session)
                },
                move |handle| {
                    // Push an initial snapshot, then one after every turn
                    // boundary, tool execution, and extraction, so the
                    // editor's DAG tracks the live marking.
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
    use gemini_adk_fluent_rs::spec::SessionSpec;

    fn assert_example_valid(name: &str, json: &str) {
        let value: serde_json::Value = serde_json::from_str(json).expect("well-formed JSON");
        let spec = SessionSpec::from_value(value).expect("parses as SessionSpec");
        let validation = spec.validate();
        assert!(
            validation.valid,
            "example '{name}' failed to compile: {:?}",
            validation.errors
        );
        for report in spec.run_tests() {
            assert!(
                report.passed,
                "example '{name}' test '{}' failed: {:?}",
                report.name, report.failures
            );
        }
    }

    #[test]
    fn bundled_examples_compile_and_pass_their_tests() {
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
