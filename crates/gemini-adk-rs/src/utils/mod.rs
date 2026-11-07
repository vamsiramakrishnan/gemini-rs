//! Utility helpers: model name parsing, platform variant detection, text
//! and timestamp helpers.

/// Model name parsing helpers.
pub mod model_name;
/// Text helpers: code-fence stripping, truncation, `{key}` templating, edit distance.
pub mod text;
/// RFC 3339 timestamps without a date-time dependency.
pub mod time;
/// Platform variant detection (Gemini API vs Vertex AI).
pub mod variant;

pub use model_name::{extract_model_name, is_gemini_model, is_gemini1_model, is_gemini2_or_above};
pub use variant::{GoogleLlmVariant, get_google_llm_variant};
