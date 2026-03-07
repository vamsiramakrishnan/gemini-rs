//! Extraction pipeline configuration methods for `Live`.

use std::future::Future;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;

use rs_adk::live::extractor::{ExtractionTrigger, LlmExtractor, TurnExtractor};
use rs_adk::llm::BaseLlm;

use super::Live;

impl Live {
    // -- Turn Extraction Pipeline --

    /// Add a turn extractor that runs an OOB LLM after each turn to extract
    /// structured data from the transcript window.
    ///
    /// Automatically enables both input and output transcription.
    /// The extraction result is stored in `State` under the type name
    /// (e.g., `"OrderState"`) and can be read via `handle.extracted::<T>(name)`.
    ///
    /// The type `T` must implement `JsonSchema` for schema-guided extraction.
    /// The window size defaults to 3 turns.
    pub fn extract_turns<T>(self, llm: Arc<dyn BaseLlm>, prompt: impl Into<String>) -> Self
    where
        T: DeserializeOwned + Serialize + schemars::JsonSchema + Send + Sync + 'static,
    {
        self.extract_turns_windowed::<T>(llm, prompt, 3)
    }

    /// Like `extract_turns` but with a custom window size.
    pub fn extract_turns_windowed<T>(
        mut self,
        llm: Arc<dyn BaseLlm>,
        prompt: impl Into<String>,
        window_size: usize,
    ) -> Self
    where
        T: DeserializeOwned + Serialize + schemars::JsonSchema + Send + Sync + 'static,
    {
        // Auto-enable transcription
        self.config = self
            .config
            .enable_input_transcription()
            .enable_output_transcription();

        // Derive name from type
        let name = std::any::type_name::<T>()
            .rsplit("::")
            .next()
            .unwrap_or("Extraction")
            .to_string();

        // Generate JSON schema from the type
        let root_schema = schemars::schema_for!(T);
        let schema = serde_json::to_value(root_schema).unwrap_or(serde_json::Value::Null);

        // Auto-register LLM for connection warming
        self.warm_up_llms.push(llm.clone());

        let extractor = LlmExtractor::new(name, llm, prompt, window_size)
            .with_schema(schema)
            .with_min_words(3);
        self.extractors.push(Arc::new(extractor));
        self
    }

    /// Like `extract_turns_windowed` but with a custom extraction trigger.
    ///
    /// Use `ExtractionTrigger::AfterToolCall` when tool calls are the primary
    /// state source, `ExtractionTrigger::Interval(n)` to reduce extraction
    /// frequency, or `ExtractionTrigger::OnPhaseChange` for phase-entry extraction.
    pub fn extract_turns_triggered<T>(
        mut self,
        llm: Arc<dyn BaseLlm>,
        prompt: impl Into<String>,
        window_size: usize,
        trigger: ExtractionTrigger,
    ) -> Self
    where
        T: DeserializeOwned + Serialize + schemars::JsonSchema + Send + Sync + 'static,
    {
        // Auto-enable transcription
        self.config = self
            .config
            .enable_input_transcription()
            .enable_output_transcription();

        let name = std::any::type_name::<T>()
            .rsplit("::")
            .next()
            .unwrap_or("Extraction")
            .to_string();

        let root_schema = schemars::schema_for!(T);
        let schema = serde_json::to_value(root_schema).unwrap_or(serde_json::Value::Null);

        self.warm_up_llms.push(llm.clone());

        let extractor = LlmExtractor::new(name, llm, prompt, window_size)
            .with_schema(schema)
            .with_min_words(3)
            .with_trigger(trigger);
        self.extractors.push(Arc::new(extractor));
        self
    }

    /// Add a custom `TurnExtractor` implementation.
    pub fn extractor(mut self, extractor: Arc<dyn TurnExtractor>) -> Self {
        // Auto-enable transcription
        self.config = self
            .config
            .enable_input_transcription()
            .enable_output_transcription();
        self.extractors.push(extractor);
        self
    }

    /// Called when a TurnExtractor produces a result.
    ///
    /// The callback receives the extractor name and the extracted JSON value.
    pub fn on_extracted<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_extracted = Some(Arc::new(move |name, value| Box::pin(f(name, value))));
        self
    }

    /// Called when a TurnExtractor fails.
    ///
    /// The callback receives the extractor name and error message.
    /// Use this for custom error handling (alerting, retry logic, etc.).
    pub fn on_extraction_error<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.callbacks.on_extraction_error =
            Some(Arc::new(move |name, error| Box::pin(f(name, error))));
        self
    }
}
