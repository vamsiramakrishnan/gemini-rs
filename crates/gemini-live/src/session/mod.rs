//! Session orchestration — the central coordination layer.
//!
//! Provides [`SessionHandle`] (the public API surface), [`SessionEvent`] (events from the server),
//! [`SessionCommand`] (commands to the server), and turn tracking.

pub mod errors;
pub mod events;
pub mod handle;
pub mod state;
pub mod traits;

// Re-export all public types at the `session::` level for backward compatibility.
pub use errors::{AuthError, SessionError, SetupError, WebSocketError};
pub use events::{recv_event, ResumeInfo, SessionCommand, SessionEvent, Turn};
pub use handle::SessionHandle;
pub use state::{SessionPhase, SessionState};
pub use traits::{SessionReader, SessionWriter};
