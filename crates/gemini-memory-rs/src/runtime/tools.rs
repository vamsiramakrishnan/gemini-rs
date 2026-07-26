//! The two tools the model sees.
//!
//! Memory reaches the model through function calls rather than injected
//! context, for three reasons: the model's use of a memory is visible in the
//! transcript, retrieved text is unambiguously data rather than instruction,
//! and nothing is spent on turns that never needed memory at all.

use std::sync::Arc;

use gemini_adk_rs::error::ToolError;
use gemini_adk_rs::tool::SimpleTool;
use serde_json::json;

use crate::core::{MutationIntent, TurnId};
use crate::engine::MemorySession;

/// The recall tool's name.
pub const RECALL_TOOL: &str = "recall_context";

/// The memory-management tool's name.
pub const MANAGE_TOOL: &str = "manage_memory";

/// The recall tool's description.
///
/// Deliberately narrow: a description that invites the model to call it for
/// anything produces a tool call on every turn, most of them useless.
pub const RECALL_DESCRIPTION: &str = "Retrieve relevant private context about this user — their \
preferences, relationships, routines, commitments or previous conversations. Do not use for \
general knowledge, current events, or anything visible in the camera.";

/// The management tool's description.
pub const MANAGE_DESCRIPTION: &str = "Use ONLY when the user explicitly asks you to remember, \
correct, forget or delete something about them, or asks what you remember. Never call this to \
store something the user did not ask you to store.";

/// JSON Schema for `recall_context`.
pub fn recall_parameters() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "What to look for, in the user's own terms.",
            },
            "scope": {
                "type": "string",
                "enum": ["recent", "persistent", "all"],
                "description": "Restrict to recent events, durable facts, or both.",
            },
        },
        "required": ["query"],
    })
}

/// JSON Schema for `manage_memory`.
pub fn manage_parameters() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "operation": {
                "type": "string",
                "enum": ["remember", "correct", "forget", "delete", "list"],
            },
            "statement": {
                "type": "string",
                "description": "What to remember, correct or forget, as the user put it.",
            },
        },
        "required": ["operation"],
    })
}

/// Build the `recall_context` tool for a session.
///
/// The handler is a state read on the happy path: by the time the model asks,
/// the answer was prepared while it was speaking.
pub fn recall_context_tool(session: Arc<MemorySession>) -> SimpleTool {
    SimpleTool::new(
        RECALL_TOOL,
        RECALL_DESCRIPTION,
        Some(recall_parameters()),
        move |args: serde_json::Value| {
            let session = session.clone();
            async move {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if query.trim().is_empty() {
                    return Ok(json!({ "status": "not_found", "facts": [] }));
                }
                let turn = current_turn(&session);
                Ok(session.recall(&query, turn).await)
            }
        },
    )
}

/// Build the `manage_memory` tool for a session.
pub fn manage_memory_tool(session: Arc<MemorySession>) -> SimpleTool {
    SimpleTool::new(
        MANAGE_TOOL,
        MANAGE_DESCRIPTION,
        Some(manage_parameters()),
        move |args: serde_json::Value| {
            let session = session.clone();
            async move {
                let operation = args
                    .get("operation")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let Some(intent) = parse_operation(operation) else {
                    return Err(ToolError::InvalidArgs(format!(
                        "unknown memory operation `{operation}`"
                    )));
                };
                let statement = args
                    .get("statement")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .trim()
                    .to_string();

                // Every operation but `list` needs something to act on, and
                // guessing at a deletion target is not recoverable.
                if statement.is_empty() && intent != MutationIntent::List {
                    return Ok(json!({
                        "status": "needs_clarification",
                        "operation": operation,
                        "message": "Ask the user what specifically to act on.",
                    }));
                }

                let turn = current_turn(&session);
                session
                    .apply_explicit_command(intent, &statement, turn)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
            }
        },
    )
}

fn parse_operation(raw: &str) -> Option<MutationIntent> {
    Some(match raw {
        "remember" => MutationIntent::Remember,
        "correct" => MutationIntent::Correct,
        "forget" => MutationIntent::Forget,
        "delete" => MutationIntent::Delete,
        "list" => MutationIntent::List,
        _ => return None,
    })
}

fn current_turn(session: &MemorySession) -> TurnId {
    session.active_snapshot().source_turn_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{SessionId, UserId};
    use crate::engine::MemoryEngine;
    use gemini_adk_rs::tool::ToolFunction;

    async fn session() -> Arc<MemorySession> {
        let engine = MemoryEngine::in_memory(UserId::new("usr_1"));
        let session = Arc::new(engine.begin_session(SessionId::new("ses_1")));
        session.begin_turn(TurnId(1));
        session
            .observe_final_transcript(TurnId(1), "I am pescatarian")
            .await
            .unwrap();
        session
    }

    #[tokio::test]
    async fn recall_serves_a_fact_learned_this_session() {
        let tool = recall_context_tool(session().await);
        let result = tool
            .call(json!({ "query": "dietary preference pescatarian" }))
            .await
            .unwrap();
        assert_eq!(result["status"], "found");
        assert!(result["facts"][0]["statement"]
            .as_str()
            .unwrap()
            .contains("pescatarian"));
    }

    #[tokio::test]
    async fn recall_reports_not_found_rather_than_failing() {
        let tool = recall_context_tool(session().await);
        let result = tool
            .call(json!({ "query": "what medication is prescribed" }))
            .await
            .unwrap();
        assert_eq!(result["status"], "not_found");
    }

    #[tokio::test]
    async fn an_empty_recall_query_is_answered_not_searched() {
        let tool = recall_context_tool(session().await);
        assert_eq!(
            tool.call(json!({ "query": "   " })).await.unwrap()["status"],
            "not_found"
        );
        assert_eq!(tool.call(json!({})).await.unwrap()["status"], "not_found");
    }

    #[tokio::test]
    async fn an_explicit_remember_takes_effect_in_session_and_commits_later() {
        let session = session().await;
        let tool = manage_memory_tool(session.clone());
        let result = tool
            .call(json!({
                "operation": "remember",
                "statement": "The user is allergic to shellfish."
            }))
            .await
            .unwrap();

        assert_eq!(result["status"], "accepted");
        assert_eq!(result["effective_in_session"], true);
        assert_eq!(result["durable_commit"], "pending");

        // And it is immediately retrievable.
        let recall = recall_context_tool(session)
            .call(json!({ "query": "allergic shellfish" }))
            .await
            .unwrap();
        assert_eq!(recall["status"], "found");
    }

    #[tokio::test]
    async fn listing_returns_what_is_currently_known() {
        let tool = manage_memory_tool(session().await);
        let result = tool.call(json!({ "operation": "list" })).await.unwrap();
        assert_eq!(result["operation"], "list");
        assert!(result["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str().unwrap_or_default().contains("pescatarian")));
    }

    #[tokio::test]
    async fn an_unnamed_deletion_target_asks_rather_than_guesses() {
        let tool = manage_memory_tool(session().await);
        let result = tool
            .call(json!({ "operation": "forget", "statement": "" }))
            .await
            .unwrap();
        assert_eq!(result["status"], "needs_clarification");
    }

    #[tokio::test]
    async fn an_unknown_operation_is_a_tool_error() {
        let tool = manage_memory_tool(session().await);
        assert!(tool
            .call(json!({ "operation": "obliterate" }))
            .await
            .is_err());
    }

    #[test]
    fn the_tool_descriptions_steer_away_from_indiscriminate_calls() {
        assert!(RECALL_DESCRIPTION.contains("Do not use for"));
        assert!(MANAGE_DESCRIPTION.contains("ONLY when the user explicitly asks"));
        assert_eq!(recall_parameters()["required"][0], "query");
        assert_eq!(manage_parameters()["required"][0], "operation");
    }
}
