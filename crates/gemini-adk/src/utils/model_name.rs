use once_cell::sync::Lazy;
use regex::Regex;

static MODEL_PATH_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^projects/[^/]+/locations/[^/]+/publishers/[^/]+/models/(.+)$").unwrap()
});

/// Regex to extract the major version digit from a gemini model name.
/// Matches "gemini-" followed by one or more digits (the major version).
static GEMINI_VERSION_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^gemini-(\d+)").unwrap());

/// Extract simple model name from a fully-qualified resource path.
///
/// If `model_string` matches the pattern
/// `projects/{project}/locations/{location}/publishers/{publisher}/models/{model}`,
/// returns just the `{model}` portion. Otherwise returns `model_string` as-is.
///
/// # Examples
///
/// ```
/// use gemini_adk::utils::model_name::extract_model_name;
///
/// assert_eq!(
///     extract_model_name("projects/my-proj/locations/us-central1/publishers/google/models/gemini-2.5-flash"),
///     "gemini-2.5-flash"
/// );
/// assert_eq!(extract_model_name("gemini-2.5-flash"), "gemini-2.5-flash");
/// ```
pub fn extract_model_name(model_string: &str) -> &str {
    MODEL_PATH_RE
        .captures(model_string)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str())
        .unwrap_or(model_string)
}

/// Returns `true` if the model name starts with `"gemini-"`.
///
/// The model name is first extracted from any fully-qualified resource path.
pub fn is_gemini_model(model_string: &str) -> bool {
    extract_model_name(model_string).starts_with("gemini-")
}

/// Returns `true` if the model is a Gemini 1.x model (name starts with `"gemini-1"`).
pub fn is_gemini1_model(model_string: &str) -> bool {
    extract_model_name(model_string).starts_with("gemini-1")
}

/// Returns `true` if the model is Gemini 2.x or above.
///
/// Parses the major version number from the model name. If the name does not
/// match the expected `gemini-{major}...` pattern, returns `false`.
pub fn is_gemini2_or_above(model_string: &str) -> bool {
    let name = extract_model_name(model_string);
    GEMINI_VERSION_RE
        .captures(name)
        .and_then(|caps| caps.get(1))
        .and_then(|m| m.as_str().parse::<u32>().ok())
        .map(|major| major >= 2)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_model_name ──────────────────────────────────────────

    #[test]
    fn extract_from_full_path() {
        assert_eq!(
            extract_model_name(
                "projects/my-proj/locations/us-central1/publishers/google/models/gemini-2.5-flash"
            ),
            "gemini-2.5-flash"
        );
    }

    #[test]
    fn extract_from_simple_name() {
        assert_eq!(extract_model_name("gemini-2.5-flash"), "gemini-2.5-flash");
    }

    #[test]
    fn extract_preserves_suffixes() {
        assert_eq!(
            extract_model_name("projects/p/locations/l/publishers/pub/models/gemini-1.5-pro-002"),
            "gemini-1.5-pro-002"
        );
    }

    #[test]
    fn extract_non_gemini_full_path() {
        assert_eq!(
            extract_model_name("projects/p/locations/l/publishers/pub/models/custom-model"),
            "custom-model"
        );
    }

    #[test]
    fn extract_empty_string() {
        assert_eq!(extract_model_name(""), "");
    }

    // ── is_gemini_model ─────────────────────────────────────────────

    #[test]
    fn gemini_model_simple() {
        assert!(is_gemini_model("gemini-2.5-flash"));
        assert!(is_gemini_model("gemini-1.5-pro"));
    }

    #[test]
    fn non_gemini_model() {
        assert!(!is_gemini_model("claude-3-opus"));
        assert!(!is_gemini_model("gpt-4"));
        assert!(!is_gemini_model("custom-model"));
    }

    #[test]
    fn gemini_model_full_path() {
        assert!(is_gemini_model(
            "projects/p/locations/l/publishers/pub/models/gemini-2.5-flash"
        ));
    }

    // ── is_gemini1_model ────────────────────────────────────────────

    #[test]
    fn gemini1_models() {
        assert!(is_gemini1_model("gemini-1.5-pro"));
        assert!(is_gemini1_model("gemini-1.5-flash"));
        assert!(is_gemini1_model("gemini-1.0-pro"));
    }

    #[test]
    fn gemini1_full_path() {
        assert!(is_gemini1_model(
            "projects/p/locations/l/publishers/pub/models/gemini-1.5-pro-002"
        ));
    }

    #[test]
    fn gemini2_not_gemini1() {
        assert!(!is_gemini1_model("gemini-2.5-flash"));
        assert!(!is_gemini1_model("gemini-2.0-flash"));
    }

    #[test]
    fn non_gemini_not_gemini1() {
        assert!(!is_gemini1_model("gpt-4"));
    }

    // ── is_gemini2_or_above ─────────────────────────────────────────

    #[test]
    fn gemini2_or_above_positive() {
        assert!(is_gemini2_or_above("gemini-2.5-flash"));
        assert!(is_gemini2_or_above("gemini-2.0-flash"));
        assert!(is_gemini2_or_above("gemini-3.0-ultra"));
    }

    #[test]
    fn gemini2_or_above_negative() {
        assert!(!is_gemini2_or_above("gemini-1.5-pro"));
        assert!(!is_gemini2_or_above("gemini-1.0-pro"));
    }

    #[test]
    fn gemini2_or_above_full_path() {
        assert!(is_gemini2_or_above(
            "projects/p/locations/l/publishers/pub/models/gemini-2.5-flash"
        ));
    }

    #[test]
    fn gemini2_or_above_non_gemini() {
        assert!(!is_gemini2_or_above("custom-model"));
        assert!(!is_gemini2_or_above("gpt-4"));
    }

    #[test]
    fn gemini2_or_above_edge_cases() {
        assert!(!is_gemini2_or_above(""));
        assert!(!is_gemini2_or_above("gemini-"));
    }
}
