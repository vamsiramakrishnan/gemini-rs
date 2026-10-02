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
use gemini_adk_fluent_rs::spec::{BindingAllowlist, SessionSpec, SpecResources};

use crate::app::{AppError, ClientMessage, DemoApp, ServerMessage, WsSender};
use crate::bridge::SessionBridge;
use crate::demo_meta;

/// Runs session specs authored as JSON in the Flow Studio editor.
pub struct FlowStudio;

/// What a spec posted by the browser may reach, as the operator configured
/// it. Nothing by default: MCP entries are dropped and HTTP bindings run as
/// mocks.
///
/// - `FLOW_STUDIO_ALLOW_HTTP`: comma-separated URL prefixes, e.g.
///   `https://api.example.com/,https://staging.example.com/v2/`.
/// - `FLOW_STUDIO_ALLOW_MCP`: semicolon-separated `mcp` entries, matched
///   exactly.
fn studio_allowlist() -> BindingAllowlist {
    let mut allow = BindingAllowlist::default();
    if let Ok(prefixes) = std::env::var("FLOW_STUDIO_ALLOW_HTTP") {
        for prefix in prefixes.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            allow = allow.allow_http_prefix(prefix);
        }
    }
    if let Ok(entries) = std::env::var("FLOW_STUDIO_ALLOW_MCP") {
        for entry in entries.split(';').map(str::trim).filter(|e| !e.is_empty()) {
            allow = allow.allow_mcp(entry);
        }
    }
    allow
}

fn send_flow_status(tx: &WsSender, handle: &LiveHandle) {
    if let Some(status) = handle.task_snapshot() {
        let _ = tx.send(ServerMessage::TasksStatus { status });
    }
    let Some(status) = handle.flow_snapshot() else {
        return;
    };
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
        let notice_tx = tx.clone();
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
                    let posted = SessionSpec::from_value(config).map_err(AppError::Session)?;
                    // The spec comes from the browser: it may only reach what
                    // the operator allowed (see `studio_allowlist`).
                    let (spec, disabled) = posted.sandboxed(&studio_allowlist());
                    if !disabled.is_empty() {
                        tracing::warn!(?disabled, "Studio spec bindings disabled");
                        let _ = notice_tx.send(ServerMessage::StateUpdate {
                            key: "studio:disabled_bindings".into(),
                            value: serde_json::json!(disabled),
                        });
                    }

                    let resources = SpecResources {
                        extraction_llm: spec
                            .requires_extraction()
                            .then(super::build_extraction_llm),
                        // An in-process engine per Studio session: `memory`
                        // specs run for real (ambient tools, slots, remember
                        // effects), scoped to the connection.
                        memory: spec.requires_memory().then(|| {
                            let engine = gemini_memory_rs::prelude::MemoryEngine::in_memory(
                                gemini_memory_rs::prelude::UserId::new("studio-user"),
                            );
                            let session = std::sync::Arc::new(engine.begin_session(
                                gemini_memory_rs::prelude::SessionId::new("studio-session"),
                            ));
                            std::sync::Arc::new(
                                gemini_memory_rs::runtime::SessionMemoryBinding::new(session),
                            )
                                as std::sync::Arc<dyn gemini_adk_fluent_rs::spec::MemoryBinding>
                        }),
                        ..SpecResources::default()
                    };
                    let state = State::new();
                    // The editor presents text even when the configured model
                    // responds with audio, so request the native transcript too.
                    spec.apply(
                        live.model(super::live_model()).transcription(),
                        &state,
                        &resources,
                    )
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
                                Ok(LiveEvent::TasksChanged(status)) => {
                                    let _ = status_tx.send(ServerMessage::TasksStatus { status });
                                }
                                Ok(LiveEvent::TurnComplete)
                                | Ok(LiveEvent::ToolExecution { .. })
                                | Ok(LiveEvent::Extraction { .. }) => {
                                    send_flow_status(&status_tx, &handle);
                                }
                                Ok(LiveEvent::Disconnected { .. })
                                | Err(broadcast::error::RecvError::Closed) => break,
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
    use gemini_adk_fluent_rs::tasks::{OperationStatus, TaskStatus};
    use std::collections::{BTreeMap, BTreeSet};

    #[tokio::test]
    async fn every_gallery_workflow_and_task_journey_passes() {
        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static/examples/flows");
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(directory.join("index.json")).unwrap())
                .unwrap();
        let entries = manifest["examples"].as_array().unwrap();
        let mut files = BTreeSet::new();
        let mut references = 0;
        let mut reference_tests = 0;
        for entry in entries {
            let file = entry["file"].as_str().unwrap();
            assert!(
                files.insert(file.to_owned()),
                "duplicate gallery entry {file}"
            );
            let source = std::fs::read_to_string(directory.join(file)).unwrap();
            let spec = SessionSpec::from_value(serde_json::from_str(&source).unwrap()).unwrap();
            let validation = spec.validate();
            assert!(validation.valid, "{file}: {:?}", validation.errors);
            let reports = spec.run_tests();
            assert!(
                !reports.is_empty() || !spec.task_scenarios.is_empty(),
                "{file} has no tests"
            );
            for report in &reports {
                assert!(
                    report.passed,
                    "{file}/{}: {:?}",
                    report.name, report.failures
                );
            }
            for report in spec.run_scenarios().await {
                assert!(report.passed, "{file}/{}: {:?}", report.name, report.error);
            }
            let mut completed = BTreeSet::new();
            let mut exercised = BTreeSet::new();
            for scenario in &spec.task_scenarios {
                let (trace, snapshot) = spec
                    .trace_tasks(&scenario.steps)
                    .await
                    .unwrap_or_else(|errors| panic!("{file}/{}: {errors:?}", scenario.name));
                let failures: Vec<_> = trace.iter().flat_map(|event| &event.failures).collect();
                assert!(
                    failures.is_empty(),
                    "{file}/{}: {failures:?}",
                    scenario.name
                );
                let owners: BTreeMap<_, _> = snapshot
                    .tasks
                    .iter()
                    .map(|task| (&task.id, &task.skill.name))
                    .collect();
                for task in &snapshot.tasks {
                    if task.status == TaskStatus::Completed {
                        completed.insert(task.skill.name.clone());
                    }
                }
                for operation in &snapshot.operations {
                    if operation.status == OperationStatus::Succeeded {
                        exercised.insert((
                            owners[&operation.owner.task].clone(),
                            operation.tool.clone(),
                        ));
                    }
                }
            }
            if file.starts_with("reference/") {
                references += 1;
                reference_tests += reports.len();
                assert!(
                    spec.skills.is_empty(),
                    "original reference unexpectedly migrated: {file}"
                );
            } else {
                assert!(
                    spec.skills.len() >= 4,
                    "{file} needs independent capabilities"
                );
                assert!(
                    validation.warnings.is_empty(),
                    "{file}: {:?}",
                    validation.warnings
                );
                for skill in &spec.skills {
                    assert!(
                        completed.contains(&skill.name),
                        "{file}/{} never completes in a task scenario",
                        skill.name
                    );
                    for tool in &skill.tools {
                        assert!(
                            exercised.contains(&(skill.name.clone(), tool.tool.name.clone())),
                            "{file}/{} tool {} has no successful task scenario",
                            skill.name,
                            tool.tool.name
                        );
                    }
                }
            }
        }
        assert_eq!(references, 6, "all original workflows stay discoverable");
        assert_eq!(
            reference_tests, 20,
            "retain the original workflow test suite"
        );
    }
}
