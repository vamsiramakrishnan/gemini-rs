//! Conversation flow control — barge-in handling and turn detection.

pub mod barge_in;
pub mod turn_detection;

pub use barge_in::{BargeInAction, BargeInConfig, BargeInDetector};
pub use turn_detection::{TurnDetectionConfig, TurnDetectionEvent, TurnDetector};
