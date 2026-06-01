//! Demo-app vocabulary.
//!
//! The WebSocket wire protocol ([`ServerMessage`] / [`ClientMessage`]), the
//! agent-session trait ([`DemoApp`], an alias for
//! [`gemini_adk_server_rs::ws::AgentSource`]), and the registry now live in the
//! shared `gemini-adk-server-rs` core so the web app, the REST API, and the
//! `adk web` CLI share a single implementation. They are re-exported here so
//! the demo apps continue to refer to `crate::app::*`.

pub use gemini_adk_server_rs::ws::{
    AgentSource as DemoApp, AppCategory, AppError, AppInfo, AppRegistry, ClientMessage,
    ServerMessage, WsSender,
};

/// Generate `DemoApp` metadata methods from a declarative block.
///
/// Usage:
/// ```ignore
/// demo_meta! {
///     name: "voice-chat",
///     description: "Native audio voice chat with Gemini Live",
///     category: Basic,
///     features: ["voice", "transcription"],
///     tips: ["Click the microphone button to start speaking"],
///     try_saying: ["Hello! Tell me a joke."],
/// }
/// ```
#[macro_export]
macro_rules! demo_meta {
    (
        name: $name:literal,
        description: $desc:literal,
        category: $cat:ident,
        features: [$($feat:literal),* $(,)?],
        $(tips: [$($tip:literal),* $(,)?],)?
        $(try_saying: [$($try:literal),* $(,)?],)?
    ) => {
        fn name(&self) -> &str { $name }
        fn description(&self) -> &str { $desc }
        fn category(&self) -> $crate::app::AppCategory { $crate::app::AppCategory::$cat }
        fn features(&self) -> Vec<String> { vec![$($feat.into()),*] }
        fn tips(&self) -> Vec<String> { vec![$($($tip.into()),*)?] }
        fn try_saying(&self) -> Vec<String> { vec![$($($try.into()),*)?] }
    };
}
