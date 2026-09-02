//! # 35 — Session Persistence
//!
//! Two complementary persistence layers that survive process restarts:
//!
//! **`SessionPersistence` / `SessionSnapshot`** (Live session snapshots)
//! - Persists the control-plane state of an active Live voice session:
//!   State key-values, current phase, turn count, transcript summary, and
//!   the Gemini server resume handle.
//! - Built-in backends: `MemoryPersistence` (tests) and `FsPersistence`
//!   (filesystem, single-server deployments).
//! - Custom backends implement the three-method `SessionPersistence` trait
//!   (async save / load / delete).
//! - Used in `Live::builder()` via `.persistence(Arc::new(...))` +
//!   `.session_id("user-123-session-456")`.
//!
//! **`SessionService` / `InMemorySessionService`** (Multi-session event log)
//! - ADK-JS-parity session CRUD with an append-only event log.
//! - `create_session` \u{2192} `append_event` \u{2192} `get_events` forms the audit trail
//!   of an agent invocation (user messages, model turns, tool calls, state
//!   mutations).
//! - `InMemorySessionService` is lock-free (DashMap), suitable for testing
//!   and single-process deployments. For persistence across restarts use
//!   `SqliteSessionService` (built-in, no feature flag) or the
//!   feature-gated `PostgresSessionService` / `VertexAiSessionService`.
//!
//! Runs without credentials or external servers.

use std::collections::HashMap;
use std::sync::Arc;

use gemini_adk_fluent_rs::live::SessionSnapshot;
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_rs::events::Event;
use gemini_adk_rs::{InMemorySessionService, Session, SessionService};
use serde_json::json;

#[tokio::main]
async fn main() {
    println!("=== 35: Session Persistence ===\n");

    // ────────────────────────────────────────────────────────────────────────
    // Part A: MemoryPersistence — Live session snapshot round-trip
    // ────────────────────────────────────────────────────────────────────────
    println!("--- A. MemoryPersistence (Live session snapshots) ---\n");

    let store = MemoryPersistence::new();

    // Build a snapshot representing the saved state of a live voice session.
    // In production the SDK fills this automatically; here we build it manually
    // to exercise the persistence API directly.
    let snapshot = SessionSnapshot {
        state: {
            let mut m = HashMap::new();
            m.insert("customer_id".to_string(), json!("C-12345"));
            m.insert("session:turn_count".to_string(), json!(7));
            m.insert("derived:risk".to_string(), json!(0.25));
            m
        },
        phase: "order_confirmation".to_string(),
        turn_count: 7,
        transcript_summary: "User confirmed item SKU-99 and delivery address.".to_string(),
        resume_handle: Some("opaque-server-handle-abc".to_string()),
        saved_at: "2026-05-30T10:00:00Z".to_string(),
    };

    // Save the snapshot.
    store
        .save("user-alice-session-1", &snapshot)
        .await
        .expect("save should succeed");
    println!("  Saved snapshot for session 'user-alice-session-1'.");

    // Load it back.
    let loaded = store
        .load("user-alice-session-1")
        .await
        .expect("load should succeed")
        .expect("snapshot must be present after save");

    assert_eq!(loaded.phase, "order_confirmation");
    assert_eq!(loaded.turn_count, 7);
    assert_eq!(
        loaded.resume_handle.as_deref(),
        Some("opaque-server-handle-abc")
    );
    assert_eq!(loaded.state.get("customer_id"), Some(&json!("C-12345")));
    println!(
        "  \u{2713} Loaded: phase={}, turns={}",
        loaded.phase, loaded.turn_count
    );
    println!(
        "  \u{2713} resume_handle = {:?}",
        loaded.resume_handle.as_deref()
    );

    // Loading a non-existent session returns None (not an error).
    let missing = store
        .load("no-such-session")
        .await
        .expect("should not error");
    assert!(missing.is_none());
    println!("  \u{2713} Missing session returns None (no error)");

    // Delete the session.
    store
        .delete("user-alice-session-1")
        .await
        .expect("delete should succeed");
    let after_delete = store
        .load("user-alice-session-1")
        .await
        .expect("load after delete should not error");
    assert!(after_delete.is_none());
    println!("  \u{2713} After delete: snapshot is gone\n");

    // ────────────────────────────────────────────────────────────────────────
    // Part B: FsPersistence — filesystem snapshot round-trip
    // ────────────────────────────────────────────────────────────────────────
    println!("--- B. FsPersistence (filesystem) ---\n");

    // FsPersistence writes JSON files: <dir>/<session_id>.json
    // It creates the directory on first save.
    let tmp_dir = std::env::temp_dir().join("gemini_rs_cookbook_35");
    let fs_store = FsPersistence::new(&tmp_dir);

    let snap2 = SessionSnapshot {
        state: {
            let mut m = HashMap::new();
            m.insert("lang".to_string(), json!("en"));
            m
        },
        phase: "greeting".to_string(),
        turn_count: 1,
        transcript_summary: String::new(),
        resume_handle: None,
        saved_at: "2026-05-30T10:05:00Z".to_string(),
    };

    fs_store
        .save("fs-session-abc", &snap2)
        .await
        .expect("fs save");
    let fs_loaded = fs_store
        .load("fs-session-abc")
        .await
        .expect("fs load")
        .expect("must be present");
    assert_eq!(fs_loaded.phase, "greeting");
    println!(
        "  \u{2713} FsPersistence round-trip: phase={}",
        fs_loaded.phase
    );

    // Cleanup temp files.
    fs_store.delete("fs-session-abc").await.expect("fs delete");
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    println!("  \u{2713} Cleaned up temp files\n");

    // ────────────────────────────────────────────────────────────────────────
    // Part C: How to wire persistence into a Live session (no credentials)
    // ────────────────────────────────────────────────────────────────────────
    println!("--- C. Wiring persistence into Live::builder() ---\n");

    let _persistence = Arc::new(MemoryPersistence::new());

    // In a real app:
    //
    //   let handle = Live::builder()
    //       .model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO)
    //       .persistence(Arc::new(FsPersistence::new("/tmp/sessions")))
    //       .session_id("user-123-session-456")
    //       .connect_from_env()
    //       .await?;
    //
    // The SDK auto-saves a snapshot on TurnComplete and auto-loads on connect
    // when session_id matches an existing snapshot. The resume_handle from the
    // previous snapshot is sent in the setup message so the Gemini server
    // resumes the conversation context.

    println!("  MemoryPersistence  \u{2192} in-process, lost on restart (tests/prototyping)");
    println!("  FsPersistence      \u{2192} JSON files in a directory (single-server deployments)");
    println!(
        "  Custom backend     \u{2192} implement SessionPersistence trait (Redis, DynamoDB, ...)"
    );
    println!();
    println!("  // Wire into Live::builder():");
    println!("  // .persistence(Arc::new(FsPersistence::new(\"/tmp/sessions\")))");
    println!("  // .session_id(\"user-123-session-456\")");
    println!();

    // ────────────────────────────────────────────────────────────────────────
    // Part D: InMemorySessionService — ADK-JS-parity event-log round-trip
    // ────────────────────────────────────────────────────────────────────────
    println!("--- D. InMemorySessionService (multi-session event log) ---\n");

    let svc = InMemorySessionService::new();

    // Create two sessions for the same app + user.
    let session_a = svc
        .create_session("order-bot", "user-alice")
        .await
        .expect("create session A");
    let _session_b = svc
        .create_session("order-bot", "user-alice")
        .await
        .expect("create session B");

    println!("  Created sessions: {} and ...", session_a.id);

    // Append events to session A.
    let user_turn = Event::new("user", Some("I'd like to return item SKU-99".to_string()));
    let agent_turn = Event::new(
        "order-bot",
        Some("I can help with that. Can you confirm your order number?".to_string()),
    );

    svc.append_event(&session_a.id, user_turn)
        .await
        .expect("append user turn");
    svc.append_event(&session_a.id, agent_turn)
        .await
        .expect("append agent turn");

    // Retrieve events.
    let events = svc.get_events(&session_a.id).await.expect("get events");
    assert_eq!(events.len(), 2);
    println!("  \u{2713} Session A has {} events", events.len());
    for (i, ev) in events.iter().enumerate() {
        println!(
            "    [{}] author={:?}  content={:?}",
            i + 1,
            ev.author,
            ev.content.as_deref().unwrap_or("")
        );
    }

    // List sessions for the same app+user pair.
    let sessions = svc
        .list_sessions("order-bot", "user-alice")
        .await
        .expect("list sessions");
    assert_eq!(sessions.len(), 2, "two sessions created");
    println!(
        "  \u{2713} list_sessions returned {} sessions for user-alice",
        sessions.len()
    );

    // get_session by ID.
    let fetched: Option<Session> = svc.get_session(&session_a.id).await.expect("get_session");
    assert!(fetched.is_some());
    println!("  \u{2713} get_session by ID succeeded");

    // Delete one session; it disappears from the list.
    svc.delete_session(&session_a.id)
        .await
        .expect("delete session A");
    let remaining = svc
        .list_sessions("order-bot", "user-alice")
        .await
        .expect("list after delete");
    assert_eq!(remaining.len(), 1, "one session remains");
    println!(
        "  \u{2713} After delete: {} session remains\n",
        remaining.len()
    );

    // ────────────────────────────────────────────────────────────────────────
    // Part E: Other backends (feature-gated)
    // ────────────────────────────────────────────────────────────────────────
    println!("--- E. Other SessionService backends (feature-gated) ---\n");

    // SqliteSessionService — built-in, no feature flag, file or in-memory DB:
    //
    //   use gemini_adk_rs::session::{SqliteSessionService, SqliteSessionConfig};
    //   let svc = SqliteSessionService::connect(SqliteSessionConfig::memory()).await?;
    //
    // PostgresSessionService — requires feature "postgres-sessions":
    //
    //   use gemini_adk_rs::session::{PostgresSessionService, PostgresSessionConfig};
    //   let svc = PostgresSessionService::connect(
    //       PostgresSessionConfig::new("postgres://localhost/mydb")
    //   ).await?;
    //
    // VertexAiSessionService — requires feature "vertex-ai-sessions":
    //
    //   use gemini_adk_rs::session::{VertexAiSessionService, VertexAiSessionConfig};
    //   let svc = VertexAiSessionService::new(
    //       VertexAiSessionConfig::new(project, location, token)
    //   );
    //
    // All backends implement the same SessionService trait, so you can swap
    // the backend without changing any application logic.

    println!("  SqliteSessionService   \u{2192} built-in (no feature flag), file or in-memory DB");
    println!("  PostgresSessionService \u{2192} feature = \"postgres-sessions\"");
    println!("  VertexAiSessionService \u{2192} feature = \"vertex-ai-sessions\"");
    println!("  All implement SessionService \u{2014} swap backends without changing app logic.");

    println!("\n=== Done ===");
}
