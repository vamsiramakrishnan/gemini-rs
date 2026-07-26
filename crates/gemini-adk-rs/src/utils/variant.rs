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
    classify(std::env::var("GOOGLE_GENAI_USE_VERTEXAI").ok().as_deref())
}

/// The rule itself, separated from where the value comes from.
///
/// This exists so the rule can be tested without touching the process
/// environment. Environment variables are global mutable state shared by every
/// thread in the test binary, and `cargo test` runs tests in parallel: a set of
/// tests that each set the same variable and then read it back will pass
/// locally for months and fail on a busier machine, having read a value another
/// test wrote microseconds earlier.
fn classify(value: Option<&str>) -> GoogleLlmVariant {
    match value {
        Some(value) => {
            let lower = value.to_lowercase();
            if lower == "true" || lower == "1" {
                GoogleLlmVariant::VertexAi
            } else {
                GoogleLlmVariant::GeminiApi
            }
        }
        None => GoogleLlmVariant::GeminiApi,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule, exhaustively, with no global state involved.
    ///
    /// These replace seven tests that each set `GOOGLE_GENAI_USE_VERTEXAI` and
    /// read it back. That worked whenever the scheduler happened to keep them
    /// apart and failed when it did not — on CI, `vertex_ai_true_lowercase`
    /// asserted `VertexAi` and got `GeminiApi`, having read the value a
    /// concurrent test had just written. The helper's own comment argued it was
    /// safe "because each test uses a unique value"; the values were unique but
    /// the *variable* was one, shared by every thread in the binary.
    #[test]
    fn truthy_values_select_vertex() {
        for value in ["true", "TRUE", "True", "1"] {
            assert_eq!(
                classify(Some(value)),
                GoogleLlmVariant::VertexAi,
                "{value:?} should select Vertex"
            );
        }
    }

    #[test]
    fn everything_else_selects_the_gemini_api() {
        for value in ["false", "0", "", "yes", "vertex", "TRUEISH"] {
            assert_eq!(
                classify(Some(value)),
                GoogleLlmVariant::GeminiApi,
                "{value:?} should select the Gemini API"
            );
        }
    }

    #[test]
    fn an_unset_variable_selects_the_gemini_api() {
        assert_eq!(classify(None), GoogleLlmVariant::GeminiApi);
    }

    /// One test that the wiring reads the variable at all.
    ///
    /// Deliberately the only test in this module that touches the environment,
    /// which is what makes it safe: with a single writer there is nobody to
    /// race. It asserts the plumbing, not the rule — the rule is covered above.
    #[test]
    fn the_variable_is_the_one_that_is_read() {
        let restore = std::env::var("GOOGLE_GENAI_USE_VERTEXAI").ok();
        std::env::set_var("GOOGLE_GENAI_USE_VERTEXAI", "true");
        let observed = get_google_llm_variant();
        match restore {
            Some(value) => std::env::set_var("GOOGLE_GENAI_USE_VERTEXAI", value),
            None => std::env::remove_var("GOOGLE_GENAI_USE_VERTEXAI"),
        }
        assert_eq!(observed, GoogleLlmVariant::VertexAi);
    }
}
