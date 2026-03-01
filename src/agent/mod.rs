//! Agent framework — function registry, ADK bridge, and A2A client.

pub mod a2a_client;
pub mod adk_bridge;
pub mod function_registry;

pub use a2a_client::{A2AClient, A2AConfig, A2ATask, AgentCard, AgentSkill};
pub use adk_bridge::{AdkBridge, AdkConfig};
pub use function_registry::FunctionRegistry;

use thiserror::Error;

/// Errors from agent framework operations.
#[derive(Debug, Error, Clone)]
pub enum AgentError {
    /// Failed to dispatch a function call to the agent backend.
    #[error("Dispatch failed: {0}")]
    DispatchFailed(String),

    /// Failed to parse the agent response.
    #[error("Response parse error: {0}")]
    ResponseParse(String),

    /// Agent has not been discovered yet (call `discover()` first).
    #[error("Agent not discovered — call discover() first")]
    NotDiscovered,

    /// Agent card discovery failed.
    #[error("Discovery failed: {0}")]
    DiscoveryFailed(String),
}
