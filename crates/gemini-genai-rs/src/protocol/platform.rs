//! Platform abstraction — Google AI vs Vertex AI URL/version logic.

use crate::protocol::types::GeminiModel;

/// Which platform variant to use for the Gemini API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Platform {
    /// Google AI (generativelanguage.googleapis.com)
    GoogleAI,
    /// Vertex AI (aiplatform.googleapis.com)
    VertexAI {
        /// Google Cloud project ID.
        project: String,
        /// Regional location (e.g. `"us-central1"` or `"global"`).
        location: String,
    },
}

impl Platform {
    /// The base hostname for API requests.
    pub fn base_host(&self) -> String {
        match self {
            Platform::GoogleAI => "generativelanguage.googleapis.com".to_string(),
            Platform::VertexAI { location, .. } => {
                if location == "global" {
                    "aiplatform.googleapis.com".to_string()
                } else {
                    format!("{location}-aiplatform.googleapis.com")
                }
            }
        }
    }

    /// The API version string.
    pub fn api_version(&self) -> &str {
        match self {
            Platform::GoogleAI => "v1beta",
            Platform::VertexAI { .. } => "v1beta1",
        }
    }

    /// Build the model URI for the setup message.
    pub fn model_uri(&self, model: &GeminiModel) -> String {
        match self {
            Platform::GoogleAI => model.to_string(), // "models/..."
            Platform::VertexAI { project, location } => {
                let model_id = model.to_string().trim_start_matches("models/").to_string();
                format!(
                    "projects/{project}/locations/{location}/publishers/google/models/{model_id}"
                )
            }
        }
    }

    /// The WebSocket service path.
    pub fn ws_path(&self) -> &str {
        match self {
            Platform::GoogleAI => {
                "google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent"
            }
            Platform::VertexAI { .. } => {
                "google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::GeminiModel;

    #[test]
    fn google_ai_base_host() {
        assert_eq!(
            Platform::GoogleAI.base_host(),
            "generativelanguage.googleapis.com"
        );
    }

    #[test]
    fn google_ai_api_version() {
        assert_eq!(Platform::GoogleAI.api_version(), "v1beta");
    }

    #[test]
    fn vertex_ai_api_version() {
        let p = Platform::VertexAI {
            project: "p".into(),
            location: "us-central1".into(),
        };
        assert_eq!(p.api_version(), "v1beta1");
    }

    #[test]
    fn vertex_ai_base_host_regional() {
        let p = Platform::VertexAI {
            project: "p".into(),
            location: "us-central1".into(),
        };
        assert_eq!(p.base_host(), "us-central1-aiplatform.googleapis.com");
    }

    #[test]
    fn vertex_ai_base_host_global() {
        let p = Platform::VertexAI {
            project: "p".into(),
            location: "global".into(),
        };
        assert_eq!(p.base_host(), "aiplatform.googleapis.com");
    }

    #[test]
    fn google_ai_model_uri() {
        let uri = Platform::GoogleAI.model_uri(&GeminiModel::Gemini2_0FlashLive);
        assert_eq!(uri, "models/gemini-2.0-flash-live-001");
    }

    #[test]
    fn vertex_ai_model_uri() {
        let p = Platform::VertexAI {
            project: "my-proj".into(),
            location: "us-central1".into(),
        };
        let uri = p.model_uri(&GeminiModel::Gemini2_0FlashLive);
        assert!(uri.contains("projects/my-proj/locations/us-central1/publishers/google/models/gemini-2.0-flash-live-001"));
    }

    #[test]
    fn google_ai_ws_path() {
        assert!(Platform::GoogleAI.ws_path().contains("GenerativeService"));
    }

    #[test]
    fn vertex_ai_ws_path() {
        let p = Platform::VertexAI {
            project: "p".into(),
            location: "x".into(),
        };
        assert!(p.ws_path().contains("LlmBidiService"));
    }

    #[test]
    fn platform_is_clone_and_debug() {
        let p = Platform::GoogleAI;
        let _p2 = p.clone();
        let _s = format!("{:?}", p);
    }
}
