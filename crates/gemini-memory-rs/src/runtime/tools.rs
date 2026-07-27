//! The two tools the model sees.
//!
//! Memory reaches the model through function calls rather than injected
//! context, for three reasons: the model's use of a memory is visible in the
//! transcript, retrieved text is unambiguously data rather than instruction,
//! and nothing is spent on turns that never needed memory at all.
//!
//! Both are [`TypedTool`]s over argument structs, so the JSON Schema the model
//! is constrained by is generated from the types the handler actually decodes.
//! `manage_memory`'s `operation` is the domain's own [`MutationIntent`], which
//! means the tool contract and the ledger cannot disagree about what operations
//! exist.

use std::sync::Arc;

use gemini_adk_rs::error::ToolError;
use gemini_adk_rs::tool::TypedTool;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::core::{MemoryKind, MutationIntent, TurnId};
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

/// Which slice of memory a recall should search.
///
/// These doc comments are not documentation for a reader; they are the schema
/// descriptions the *model* chooses from, and the choice is a hard filter. A
/// model that picks the wrong slice does not get a worse answer, it gets no
/// answer — while lower-relevance records from the slice it did pick come back
/// looking like the best memory has. So each variant says what it *excludes*,
/// in the vocabulary of a question rather than of this crate's taxonomy.
///
/// Observed before that was true: asked "what did I promise to bring to the
/// housewarming", the model chose `persistent` — a promise feels like a durable
/// fact — which filters out `Commitment`, the kind the answer was filed under.
/// The commitment was excluded, another guest's plans came back instead, and
/// the model correctly reported that it did not know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecallScope {
    /// Only memories with a time attached: things that happened, plans,
    /// promises and commitments. Excludes standing facts about the person.
    Recent,
    /// Only timeless facts: identity, preferences, relationships, routines,
    /// how they like to be spoken to. Excludes anything that happened, and
    /// excludes every promise, plan and commitment.
    Persistent,
    /// Everything. Choose this unless the question is explicitly limited to one
    /// of the other two.
    #[default]
    All,
}

impl RecallScope {
    /// The memory kinds this scope admits; empty means no restriction.
    pub fn kinds(self) -> Vec<MemoryKind> {
        match self {
            Self::All => Vec::new(),
            Self::Recent => vec![MemoryKind::Episodic, MemoryKind::Commitment],
            Self::Persistent => vec![
                MemoryKind::Identity,
                MemoryKind::Preference,
                MemoryKind::Relationship,
                MemoryKind::RelationshipPreference,
                MemoryKind::Routine,
                MemoryKind::CommunicationStyle,
                MemoryKind::LocationPreference,
            ],
        }
    }
}

/// Arguments to `recall_context`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecallArgs {
    /// What to look for, in the user's own terms.
    pub query: String,
    /// Which slice of memory to search.
    ///
    /// Every value is spelled out here rather than on the variants because
    /// **per-variant descriptions do not reach the model**: narrowing the
    /// derived schema to the API's subset flattens `oneOf`-of-`enum` down to a
    /// bare `{"enum": [...]}`, and the doc comment on each variant goes with
    /// it. This field's description is the only text the model actually reads
    /// about what the values mean, so it carries all of it.
    ///
    /// Omit unless the question is explicitly about one slice — narrowing
    /// wrongly excludes the answer outright rather than ranking it lower, and
    /// plausible records from the chosen slice arrive in its place. `recent`:
    /// only memories with a time attached — things that happened, plans,
    /// promises, commitments. `persistent`: only timeless facts — identity,
    /// preferences, relationships, routines; excludes everything that happened
    /// and every promise. `all`: everything, and the right choice by default.
    #[serde(default)]
    pub scope: RecallScope,
    /// Whose fact this is.
    ///
    /// Use a value from the memory map in your instructions, or omit it. This
    /// narrows nothing away — a record that does not match is ranked lower, not
    /// removed — so a wrong guess costs about one result, while a right one is
    /// worth several. Guessing is better than omitting.
    ///
    /// Distinct from who the question *mentions*: "where am I collecting
    /// Priya's cake" is a fact about the user that mentions Priya, so `about`
    /// is the user.
    #[serde(default)]
    pub about: Option<String>,
    /// Which attribute of them — for example a coffee order, a barber, an
    /// allergy.
    ///
    /// Use a value from the memory map in your instructions, or omit it. Same
    /// soft behaviour as `about`, and the more useful of the two.
    #[serde(default)]
    pub attribute: Option<String>,
}

/// Arguments to `manage_memory`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ManageArgs {
    /// What the user asked for.
    pub operation: MutationIntent,
    /// What to remember, correct or forget, as the user put it.
    #[serde(default)]
    pub statement: Option<String>,
}

/// Build the `recall_context` tool for a session.
///
/// The handler is a state read on the happy path: by the time the model asks,
/// the answer was prepared while it was speaking.
pub fn recall_context_tool(session: Arc<MemorySession>) -> TypedTool<RecallArgs> {
    TypedTool::new(RECALL_TOOL, RECALL_DESCRIPTION, move |args: RecallArgs| {
        let session = session.clone();
        async move {
            if args.query.trim().is_empty() {
                return Ok(json!({ "status": "not_found", "facts": [] }));
            }
            let turn = current_turn(&session);
            Ok(session
                .recall_scoped(&args.query, turn, args.scope, args.about, args.attribute)
                .await)
        }
    })
}

/// Build the `manage_memory` tool for a session.
pub fn manage_memory_tool(session: Arc<MemorySession>) -> TypedTool<ManageArgs> {
    TypedTool::new(MANAGE_TOOL, MANAGE_DESCRIPTION, move |args: ManageArgs| {
        let session = session.clone();
        async move {
            let statement = args.statement.unwrap_or_default().trim().to_string();

            // Every operation but `list` needs something to act on, and
            // guessing at a deletion target is not recoverable.
            if statement.is_empty() && args.operation != MutationIntent::List {
                return Ok(json!({
                    "status": "needs_clarification",
                    "operation": args.operation,
                    "message": "Ask the user what specifically to act on.",
                }));
            }

            let turn = current_turn(&session);
            session
                .apply_explicit_command(args.operation, &statement, turn)
                .await
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
        }
    })
}

fn current_turn(session: &MemorySession) -> TurnId {
    session.current_turn()
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
            .observe_final_transcript(TurnId(2), "I am meeting Kushal for dinner tonight")
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
    async fn scope_restricts_which_kinds_can_come_back() {
        let tool = recall_context_tool(session().await);

        let recent = tool
            .call(json!({ "query": "dinner pescatarian", "scope": "recent" }))
            .await
            .unwrap();
        assert!(
            !recent.to_string().contains("pescatarian"),
            "a durable preference leaked into a recent-only recall: {recent}"
        );

        let persistent = tool
            .call(json!({ "query": "dinner pescatarian", "scope": "persistent" }))
            .await
            .unwrap();
        assert!(persistent.to_string().contains("pescatarian"));
    }

    #[tokio::test]
    async fn an_omitted_scope_searches_everything() {
        let tool = recall_context_tool(session().await);
        let result = tool.call(json!({ "query": "pescatarian" })).await.unwrap();
        assert_eq!(result["status"], "found");
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
    }

    #[tokio::test]
    async fn a_missing_required_argument_is_a_tool_error() {
        let tool = recall_context_tool(session().await);
        assert!(tool.call(json!({})).await.is_err());
    }

    #[tokio::test]
    async fn an_explicit_remember_takes_effect_in_session_and_commits_later() {
        let session = session().await;
        let result = manage_memory_tool(session.clone())
            .call(json!({
                "operation": "remember",
                "statement": "The user is allergic to shellfish."
            }))
            .await
            .unwrap();

        assert_eq!(result["status"], "accepted");
        assert_eq!(result["effective_in_session"], true);
        assert_eq!(result["durable_commit"], "pending");

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
        let result = tool.call(json!({ "operation": "forget" })).await.unwrap();
        assert_eq!(result["status"], "needs_clarification");
    }

    #[tokio::test]
    async fn an_operation_outside_the_schema_is_a_tool_error() {
        let tool = manage_memory_tool(session().await);
        assert!(tool
            .call(json!({ "operation": "obliterate" }))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn the_generated_schemas_match_the_handlers() {
        let schema = recall_context_tool(session().await)
            .parameters()
            .expect("recall has parameters");
        assert!(schema["properties"]["query"].is_object());
        assert!(schema["properties"]["scope"].is_object());

        // The operation enum is the domain's, so the tool contract and the
        // ledger cannot disagree about what operations exist.
        let rendered = manage_memory_tool(session().await)
            .parameters()
            .expect("manage has parameters")
            .to_string();
        for operation in ["remember", "correct", "forget", "delete", "list"] {
            assert!(rendered.contains(operation), "schema omits `{operation}`");
        }
    }

    #[tokio::test]
    async fn what_each_scope_means_survives_into_the_schema() {
        // Schema narrowing flattens `oneOf`-of-`enum` to a bare `enum`, which
        // drops the doc comment on every variant — so anything said about a
        // value on the variant itself never reaches the model. It has to live
        // in the field description, and this is what says so.
        //
        // It matters because a wrong `scope` is a hard filter, not a worse
        // ranking: the answer is excluded while plausible records from the
        // chosen slice arrive in its place. A live run lost a commitment
        // exactly this way, the model having reasoned that a promise is a
        // durable fact.
        let scope = recall_context_tool(session().await)
            .parameters()
            .expect("recall has parameters")["properties"]["scope"]["description"]
            .as_str()
            .expect("the scope argument is described")
            .to_lowercase();

        for value in ["recent", "persistent", "all"] {
            assert!(
                scope.contains(value),
                "`{value}` is a value the model must choose between, and this \
                 description is the only place it can learn what the value \
                 means: {scope}"
            );
        }
        assert!(
            scope.contains("excludes"),
            "the description must say what narrowing leaves out, or the model \
             cannot tell that it costs the answer: {scope}"
        );
        assert!(
            scope.contains("omit unless"),
            "the description must tell the model to leave the scope unset by \
             default: {scope}"
        );
    }

    #[test]
    fn the_tool_descriptions_steer_away_from_indiscriminate_calls() {
        assert!(RECALL_DESCRIPTION.contains("Do not use for"));
        assert!(MANAGE_DESCRIPTION.contains("ONLY when the user explicitly asks"));
    }

    #[test]
    fn recall_scopes_partition_durable_from_episodic() {
        assert!(RecallScope::All.kinds().is_empty());
        assert!(RecallScope::Recent.kinds().contains(&MemoryKind::Episodic));
        assert!(RecallScope::Persistent
            .kinds()
            .contains(&MemoryKind::Preference));
        assert!(!RecallScope::Persistent
            .kinds()
            .contains(&MemoryKind::Episodic));
    }
}
