//! # The L0 contract — frames on a wire
//!
//! This module is the *engineered surface* of the wire layer: the closed set
//! of primitives L0 exposes upward, curated and named. Everything a higher
//! layer may rely on is here; everything absent is an implementation detail
//! that can change without notice. (`prelude` remains the convenience glob;
//! this module is the contract.)
//!
//! **L0 promises:** a duplex, authenticated, resumable frame stream to the
//! Live API — connect, write frames (audio/text/video/tool responses), read
//! events — plus the audio machinery a realtime client needs at the edge
//! (lock-free buffers, jitter absorption, VAD, barge-in detection).
//!
//! **L0 never:** interprets a conversation, holds session state, dispatches a
//! tool, or decides *when* to speak. It moves frames and tells the truth
//! about time.
//!
//! The primitives, by concern:
//!
//! | Concern | Primitives |
//! |---|---|
//! | Connect | [`SessionConfig`], [`ConnectBuilder`], [`ApiEndpoint`], [`ResumeInfo`] |
//! | Speak / listen | [`SessionHandle`], [`SessionWriter`], [`SessionReader`], [`SessionEvent`], [`SessionPhase`] |
//! | Say things | [`Content`], [`Part`], [`Role`], [`ModelId`], [`Voice`], [`Modality`] |
//! | Tools on the wire | [`Tool`], [`FunctionDeclaration`], [`FunctionCall`], [`FunctionResponse`], [`FunctionCallingBehavior`], [`FunctionResponseScheduling`] |
//! | Real time | [`SpscRing`], [`AudioJitterBuffer`], [`bytes_to_i16`], [`i16_to_bytes`], [`BargeInDetector`], [`TurnDetector`] |
//! | Access | [`AuthProvider`], [`GoogleAIAuth`], [`VertexAIAuth`], [`Transport`], [`TungsteniteTransport`], [`Codec`], [`JsonCodec`] |
//! | Truth | [`UsageMetadata`], [`SessionError`] |

pub use crate::protocol::types::{
    ApiEndpoint, Content, FunctionCall, FunctionCallingBehavior, FunctionDeclaration,
    FunctionResponse, FunctionResponseScheduling, Modality, ModelId, Part, Role, SessionConfig,
    Tool, UsageMetadata, Voice,
};

pub use crate::session::{
    ResumeInfo, SessionError, SessionEvent, SessionHandle, SessionPhase, SessionReader,
    SessionWriter,
};

pub use crate::transport::auth::{AuthProvider, GoogleAIAuth, VertexAIAuth};
pub use crate::transport::ws::{Transport, TungsteniteTransport};
pub use crate::transport::{Codec, ConnectBuilder, JsonCodec};

pub use crate::buffer::{AudioJitterBuffer, SpscRing, bytes_to_i16, i16_to_bytes};
pub use crate::turn::{BargeInDetector, TurnDetector};

#[cfg(test)]
mod contract {
    //! The drift guard: every primitive the module docs name must exist and
    //! be reachable from this path. A rename or removal fails compilation
    //! here first — the contract cannot erode silently.
    #[test]
    fn every_named_primitive_is_reachable() {
        use super::*;
        fn is_type<T: ?Sized>() {}
        is_type::<SessionConfig>();
        is_type::<ConnectBuilder>();
        is_type::<ApiEndpoint>();
        is_type::<ResumeInfo>();
        is_type::<SessionHandle>();
        is_type::<dyn SessionWriter>();
        is_type::<dyn SessionReader>();
        is_type::<SessionEvent>();
        is_type::<SessionPhase>();
        is_type::<Content>();
        is_type::<Part>();
        is_type::<Role>();
        is_type::<ModelId>();
        is_type::<Voice>();
        is_type::<Modality>();
        is_type::<Tool>();
        is_type::<FunctionDeclaration>();
        is_type::<FunctionCall>();
        is_type::<FunctionResponse>();
        is_type::<FunctionCallingBehavior>();
        is_type::<FunctionResponseScheduling>();
        is_type::<SpscRing<i16>>();
        is_type::<AudioJitterBuffer>();
        is_type::<BargeInDetector>();
        is_type::<TurnDetector>();
        is_type::<dyn AuthProvider>();
        is_type::<GoogleAIAuth>();
        is_type::<VertexAIAuth>();
        is_type::<TungsteniteTransport>();
        is_type::<dyn Codec>();
        is_type::<JsonCodec>();
        is_type::<UsageMetadata>();
        is_type::<SessionError>();
        fn transport_is_reachable<T: Transport>() {}
        let _ = transport_is_reachable::<TungsteniteTransport>;
        let _ = bytes_to_i16;
        let _ = i16_to_bytes;
    }
}
