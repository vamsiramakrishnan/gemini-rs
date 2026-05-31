//! Turn-taking — barge-in handling and turn detection (VAD-driven).

pub mod barge_in;
pub mod turn_detection;

pub use barge_in::{BargeInAction, BargeInConfig, BargeInDetector};
pub use turn_detection::{TurnDetectionConfig, TurnDetectionEvent, TurnDetector};
