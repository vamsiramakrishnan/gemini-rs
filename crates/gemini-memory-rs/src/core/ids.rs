//! Newtype identifiers for the memory engine.
//!
//! Every identifier is a distinct type so a `SessionId` can never be passed
//! where a `UserId` is expected — the memory engine's namespacing and privacy
//! guarantees depend on that distinction being enforced by the compiler rather
//! than by review.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(
            Debug,
            Clone,
            PartialEq,
            Eq,
            Hash,
            PartialOrd,
            Ord,
            Serialize,
            Deserialize,
            schemars::JsonSchema,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap an existing string as this identifier.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Mint a fresh random identifier with this type's conventional prefix.
            pub fn generate() -> Self {
                let raw = uuid::Uuid::new_v4().simple().to_string();
                Self(format!("{}_{}", $prefix, &raw[..12]))
            }

            /// Borrow the identifier as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the identifier, yielding the underlying string.
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

string_id!(
    /// The person who owns a memory namespace. Never accepted from model output.
    UserId,
    "usr"
);
string_id!(
    /// A durable canonical memory record.
    MemoryId,
    "mem"
);
string_id!(
    /// A logical conversation, which may span several transport sessions.
    SessionId,
    "ses"
);
string_id!(
    /// One Gemini Live WebSocket connection within a logical session.
    ConnectionId,
    "con"
);
string_id!(
    /// A single extracted interpretation of one user statement.
    ObservationId,
    "obs"
);
string_id!(
    /// A retrieval plan produced from a transcript.
    PlanId,
    "pln"
);
string_id!(
    /// An immutable prepared-memory snapshot.
    SnapshotId,
    "snp"
);
string_id!(
    /// An entry in the append-only memory event log.
    EventId,
    "evt"
);
string_id!(
    /// A person, place or thing referenced by memories.
    EntityId,
    "ent"
);

/// Monotonic per-logical-session turn counter.
///
/// Turn identity is assigned locally rather than derived from wire ordering:
/// the Live API delivers input transcription independently of turn boundaries,
/// so a locally-assigned turn id is the only stable correlation key.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    Default,
    schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct TurnId(pub u64);

impl TurnId {
    /// The turn id before any user turn has started.
    pub const ZERO: Self = Self(0);

    /// The next turn in sequence.
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for TurnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "turn_{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_carry_their_prefix_and_are_unique() {
        let a = MemoryId::generate();
        let b = MemoryId::generate();
        assert!(a.as_str().starts_with("mem_"));
        assert_ne!(a, b);
    }

    #[test]
    fn ids_round_trip_as_transparent_strings() {
        let id = UserId::new("usr_72ab");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"usr_72ab\"");
        assert_eq!(serde_json::from_str::<UserId>(&json).unwrap(), id);
    }

    #[test]
    fn turn_ids_advance() {
        assert_eq!(TurnId::ZERO.next(), TurnId(1));
    }
}
