//! Utility helpers for model name parsing and platform variant detection.

/// Model name parsing helpers.
pub mod model_name;
/// Platform variant detection (Gemini API vs Vertex AI).
pub mod variant;

pub use model_name::{extract_model_name, is_gemini1_model, is_gemini2_or_above, is_gemini_model};
pub use variant::{get_google_llm_variant, GoogleLlmVariant};
