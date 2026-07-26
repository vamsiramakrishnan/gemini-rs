//! Typed state keys for the memory subsystem.
//!
//! Memory state lives under a `memory:` prefix so it is visibly distinct from
//! the phase machine's `session:` and `turn:` scopes and never collides with
//! application keys. Constants rather than string literals, so a typo is a
//! compile error rather than a silently missing memory.

use gemini_adk_rs::state::StateKey;

use crate::core::TurnId;
use crate::retrieval::PreparedMemorySnapshot;
use crate::transcript::TranscriptHypothesis;

/// The in-progress transcript for the current turn.
///
/// Under `turn:` so the runtime's per-turn clearing wipes it automatically —
/// a partial transcript must never outlive the turn it belonged to.
pub const TRANSCRIPT_PARTIAL: StateKey<TranscriptHypothesis> =
    StateKey::new("turn:input_transcript_partial");

/// The finalized transcript for the current turn.
pub const TRANSCRIPT_FINAL: StateKey<String> = StateKey::new("turn:input_transcript_final");

/// The most recently prepared snapshot, awaiting the next eligible turn.
pub const PREPARED_MEMORY: StateKey<PreparedMemorySnapshot> = StateKey::new("memory:prepared");

/// The frozen snapshot the in-flight turn is being answered from.
pub const ACTIVE_TURN_MEMORY: StateKey<PreparedMemorySnapshot> =
    StateKey::new("memory:active_turn");

/// The generation counter that invalidates stale speculative work.
pub const MEMORY_GENERATION: StateKey<u64> = StateKey::new("memory:generation");

/// The turn currently being processed.
pub const CURRENT_TURN: StateKey<TurnId> = StateKey::new("memory:current_turn");

/// Number of session-overlay facts currently usable.
pub const OVERLAY_SIZE: StateKey<usize> = StateKey::new("memory:overlay_size");

/// The session overlay's revision, for cache keying.
pub const OVERLAY_REVISION: StateKey<u64> = StateKey::new("memory:overlay_revision");

/// Whether the user has issued an explicit memory command this session.
pub const PENDING_EXPLICIT_MUTATION: StateKey<bool> =
    StateKey::new("memory:pending_explicit_mutation");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_turn_keys_live_under_the_turn_scope_so_they_are_cleared() {
        assert!(TRANSCRIPT_PARTIAL.key().starts_with("turn:"));
        assert!(TRANSCRIPT_FINAL.key().starts_with("turn:"));
    }

    #[test]
    fn session_scoped_memory_keys_share_the_memory_prefix() {
        for key in [
            PREPARED_MEMORY.key(),
            ACTIVE_TURN_MEMORY.key(),
            MEMORY_GENERATION.key(),
            CURRENT_TURN.key(),
            OVERLAY_SIZE.key(),
            OVERLAY_REVISION.key(),
            PENDING_EXPLICIT_MUTATION.key(),
        ] {
            assert!(key.starts_with("memory:"), "{key} is outside the namespace");
        }
    }

    #[test]
    fn keys_are_distinct() {
        let keys = [
            TRANSCRIPT_PARTIAL.key(),
            TRANSCRIPT_FINAL.key(),
            PREPARED_MEMORY.key(),
            ACTIVE_TURN_MEMORY.key(),
            MEMORY_GENERATION.key(),
            CURRENT_TURN.key(),
            OVERLAY_SIZE.key(),
            OVERLAY_REVISION.key(),
            PENDING_EXPLICIT_MUTATION.key(),
        ];
        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
    }
}
