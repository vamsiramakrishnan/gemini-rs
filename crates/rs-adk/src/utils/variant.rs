/// The backend variant for Google LLM access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleLlmVariant {
    /// Vertex AI (enterprise, project-based).
    VertexAi,
    /// Gemini API (API-key based, consumer).
    GeminiApi,
}

/// Determine the Google LLM variant from the environment.
///
/// Reads the `GOOGLE_GENAI_USE_VERTEXAI` environment variable.
/// Returns [`GoogleLlmVariant::VertexAi`] when the variable is set to a
/// truthy value (`"true"`, `"1"`, case-insensitive), and
/// [`GoogleLlmVariant::GeminiApi`] otherwise (including when the variable
/// is unset).
pub fn get_google_llm_variant() -> GoogleLlmVariant {
    match std::env::var("GOOGLE_GENAI_USE_VERTEXAI") {
        Ok(val) => {
            let lower = val.to_lowercase();
            if lower == "true" || lower == "1" {
                GoogleLlmVariant::VertexAi
            } else {
                GoogleLlmVariant::GeminiApi
            }
        }
        Err(_) => GoogleLlmVariant::GeminiApi,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: run a closure with `GOOGLE_GENAI_USE_VERTEXAI` set to `val`,
    /// then restore the previous state. Tests using this must run serially
    /// (the `#[serial]` attribute is not strictly needed here because each
    /// test uses a unique value and env var reads are atomic, but callers
    /// should be aware of the global-state nature of env vars).
    fn with_env_var<F: FnOnce()>(val: Option<&str>, f: F) {
        // Save old value
        let old = std::env::var("GOOGLE_GENAI_USE_VERTEXAI").ok();
        // Set / remove
        match val {
            Some(v) => std::env::set_var("GOOGLE_GENAI_USE_VERTEXAI", v),
            None => std::env::remove_var("GOOGLE_GENAI_USE_VERTEXAI"),
        }
        f();
        // Restore
        match old {
            Some(v) => std::env::set_var("GOOGLE_GENAI_USE_VERTEXAI", v),
            None => std::env::remove_var("GOOGLE_GENAI_USE_VERTEXAI"),
        }
    }

    #[test]
    fn vertex_ai_true_lowercase() {
        with_env_var(Some("true"), || {
            assert_eq!(get_google_llm_variant(), GoogleLlmVariant::VertexAi);
        });
    }

    #[test]
    fn vertex_ai_true_uppercase() {
        with_env_var(Some("TRUE"), || {
            assert_eq!(get_google_llm_variant(), GoogleLlmVariant::VertexAi);
        });
    }

    #[test]
    fn vertex_ai_one() {
        with_env_var(Some("1"), || {
            assert_eq!(get_google_llm_variant(), GoogleLlmVariant::VertexAi);
        });
    }

    #[test]
    fn gemini_api_false() {
        with_env_var(Some("false"), || {
            assert_eq!(get_google_llm_variant(), GoogleLlmVariant::GeminiApi);
        });
    }

    #[test]
    fn gemini_api_zero() {
        with_env_var(Some("0"), || {
            assert_eq!(get_google_llm_variant(), GoogleLlmVariant::GeminiApi);
        });
    }

    #[test]
    fn gemini_api_unset() {
        with_env_var(None, || {
            assert_eq!(get_google_llm_variant(), GoogleLlmVariant::GeminiApi);
        });
    }

    #[test]
    fn gemini_api_empty_string() {
        with_env_var(Some(""), || {
            assert_eq!(get_google_llm_variant(), GoogleLlmVariant::GeminiApi);
        });
    }

    #[test]
    fn vertex_ai_mixed_case() {
        with_env_var(Some("True"), || {
            assert_eq!(get_google_llm_variant(), GoogleLlmVariant::VertexAi);
        });
    }
}
