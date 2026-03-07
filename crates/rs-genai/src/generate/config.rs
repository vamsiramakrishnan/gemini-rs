//! Request configuration for generateContent.

use crate::protocol::types::{Content, GenerationConfig, Part, SafetySetting, Tool, ToolConfig};

/// Configuration for a generateContent request.
///
/// Wraps the existing `GenerationConfig` plus safety settings, tools,
/// system instruction, and content turns.
#[derive(Debug, Clone)]
pub struct GenerateContentConfig {
    /// The conversation turns to send.
    pub contents: Vec<Content>,
    /// Generation parameters (temperature, top_p, max_output_tokens, etc.).
    pub generation_config: Option<GenerationConfig>,
    /// Per-category safety thresholds.
    pub safety_settings: Vec<SafetySetting>,
    /// Tools available to the model.
    pub tools: Vec<Tool>,
    /// Tool invocation configuration.
    pub tool_config: Option<ToolConfig>,
    /// System instruction (prepended to the conversation).
    pub system_instruction: Option<Content>,
}

impl GenerateContentConfig {
    /// Create a config from a simple text prompt.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            contents: vec![Content::user(text)],
            generation_config: None,
            safety_settings: vec![],
            tools: vec![],
            tool_config: None,
            system_instruction: None,
        }
    }

    /// Create a config from a list of content parts (e.g., text + image).
    pub fn from_parts(parts: Vec<Part>) -> Self {
        Self {
            contents: vec![Content {
                role: Some(crate::protocol::types::Role::User),
                parts,
            }],
            generation_config: None,
            safety_settings: vec![],
            tools: vec![],
            tool_config: None,
            system_instruction: None,
        }
    }

    /// Create a config from existing conversation contents.
    pub fn from_contents(contents: Vec<Content>) -> Self {
        Self {
            contents,
            generation_config: None,
            safety_settings: vec![],
            tools: vec![],
            tool_config: None,
            system_instruction: None,
        }
    }

    /// Set generation config.
    pub fn generation_config(mut self, config: GenerationConfig) -> Self {
        self.generation_config = Some(config);
        self
    }

    /// Set temperature.
    pub fn temperature(mut self, temp: f32) -> Self {
        self.generation_config
            .get_or_insert_with(GenerationConfig::default)
            .temperature = Some(temp);
        self
    }

    /// Set max output tokens.
    pub fn max_output_tokens(mut self, max: u32) -> Self {
        self.generation_config
            .get_or_insert_with(GenerationConfig::default)
            .max_output_tokens = Some(max);
        self
    }

    /// Set top_p.
    pub fn top_p(mut self, top_p: f32) -> Self {
        self.generation_config
            .get_or_insert_with(GenerationConfig::default)
            .top_p = Some(top_p);
        self
    }

    /// Set top_k.
    pub fn top_k(mut self, top_k: u32) -> Self {
        self.generation_config
            .get_or_insert_with(GenerationConfig::default)
            .top_k = Some(top_k);
        self
    }

    /// Add a safety setting.
    pub fn safety_setting(mut self, setting: SafetySetting) -> Self {
        self.safety_settings.push(setting);
        self
    }

    /// Add a tool.
    pub fn tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    /// Set tool config.
    pub fn tool_config(mut self, config: ToolConfig) -> Self {
        self.tool_config = Some(config);
        self
    }

    /// Set JSON output mode with an optional JSON Schema.
    ///
    /// Sets `responseMimeType` to `"application/json"` and, if a schema is
    /// provided, sets `responseJsonSchema` so the model is constrained to
    /// produce valid JSON matching the schema.
    pub fn json_output(mut self, schema: Option<serde_json::Value>) -> Self {
        let gc = self
            .generation_config
            .get_or_insert_with(GenerationConfig::default);
        gc.response_mime_type = Some("application/json".to_string());
        gc.response_json_schema = schema;
        self
    }

    /// Set system instruction from text.
    pub fn system_instruction(mut self, text: impl Into<String>) -> Self {
        self.system_instruction = Some(Content {
            role: None,
            parts: vec![Part::text(text)],
        });
        self
    }

    /// Serialize to the JSON request body expected by the REST API.
    pub fn to_request_body(&self) -> serde_json::Value {
        let mut body = serde_json::json!({
            "contents": self.contents,
        });

        if let Some(ref gc) = self.generation_config {
            body["generationConfig"] = serde_json::to_value(gc).unwrap_or_default();
        }

        if !self.safety_settings.is_empty() {
            body["safetySettings"] =
                serde_json::to_value(&self.safety_settings).unwrap_or_default();
        }

        if !self.tools.is_empty() {
            body["tools"] = serde_json::to_value(&self.tools).unwrap_or_default();
        }

        if let Some(ref tc) = self.tool_config {
            body["toolConfig"] = serde_json::to_value(tc).unwrap_or_default();
        }

        if let Some(ref si) = self.system_instruction {
            body["systemInstruction"] = serde_json::to_value(si).unwrap_or_default();
        }

        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::{HarmBlockThreshold, HarmCategory};

    #[test]
    fn from_text_basic() {
        let config = GenerateContentConfig::from_text("Hello");
        assert_eq!(config.contents.len(), 1);
        let body = config.to_request_body();
        let text = body["contents"][0]["parts"][0]["text"].as_str().unwrap();
        assert_eq!(text, "Hello");
    }

    #[test]
    fn with_temperature_and_max_tokens() {
        let config = GenerateContentConfig::from_text("Hello")
            .temperature(0.5)
            .max_output_tokens(1024);
        let body = config.to_request_body();
        assert_eq!(body["generationConfig"]["temperature"], 0.5);
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 1024);
    }

    #[test]
    fn with_safety_settings() {
        let config = GenerateContentConfig::from_text("Hello").safety_setting(SafetySetting {
            category: HarmCategory::HarmCategoryHarassment,
            threshold: HarmBlockThreshold::BlockOnlyHigh,
        });
        let body = config.to_request_body();
        assert!(body["safetySettings"].is_array());
        assert_eq!(
            body["safetySettings"][0]["category"],
            "HARM_CATEGORY_HARASSMENT"
        );
    }

    #[test]
    fn with_system_instruction() {
        let config =
            GenerateContentConfig::from_text("Hello").system_instruction("You are helpful");
        let body = config.to_request_body();
        let si = &body["systemInstruction"];
        assert!(si["parts"][0]["text"].as_str().unwrap().contains("helpful"));
    }
}
