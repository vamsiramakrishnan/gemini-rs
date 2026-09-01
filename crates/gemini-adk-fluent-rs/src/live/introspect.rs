//! Reading a configured [`Live`] builder back.
//!
//! The builder had 103 setters and no getters, which made three things
//! impossible: an extension could not assert its own wiring, a caller could not
//! see what a chain of `with_*` helpers had installed on their behalf, and
//! static checks like [`check_live`](crate::testing::check_live) had nothing to
//! read. Every accessor here is a plain borrow of already-public-in-spirit
//! configuration — nothing is computed, nothing connects.
//!
//! Tool names are the one place with a caveat worth stating: MCP, A2A, OpenAPI
//! and agent tools resolve asynchronously **at connect**, so before then
//! [`declared_tool_names`](Live::declared_tool_names) reports what is knowable
//! and [`pending_tool_count`](Live::pending_tool_count) reports how much is not.
//! Anything validating tool names must either run after connect resolution or
//! treat the deferred count as an admission of incompleteness.

use gemini_adk_rs::State;
use gemini_adk_rs::flow::{Enforcement, Flow};
use gemini_adk_rs::live::Phase;
use gemini_genai_rs::prelude::Tool;

use super::Live;

/// Pull every function name out of a set of wire tool declarations, including
/// the built-in grounding tools that carry no function declaration.
pub(crate) fn declaration_names(tools: &[Tool]) -> Vec<String> {
    let mut names = Vec::new();
    for tool in tools {
        if let Some(functions) = &tool.function_declarations {
            names.extend(functions.iter().map(|f| f.name.clone()));
        }
        if tool.google_search.is_some() {
            names.push("google_search".into());
        }
        if tool.code_execution.is_some() {
            names.push("code_execution".into());
        }
        if tool.url_context.is_some() {
            names.push("url_context".into());
        }
    }
    names
}

impl Live {
    /// Every tool name this builder can name *without connecting*.
    ///
    /// Covers config-level declarations (`google_search`, `code_execution`, …),
    /// everything registered on the dispatcher, and deferred **agent** tools,
    /// whose names are known up front even though the tool is built at connect.
    ///
    /// Does **not** cover MCP/A2A/OpenAPI tools, whose names live on the far
    /// side of an async handshake — see [`pending_tool_count`](Self::pending_tool_count).
    /// Duplicates are removed; order is not meaningful.
    pub fn declared_tool_names(&self) -> Vec<String> {
        let mut names = declaration_names(&self.config.tools);
        if let Some(dispatcher) = &self.dispatcher {
            names.extend(declaration_names(&dispatcher.to_tool_declarations()));
        }
        names.extend(self.deferred_agent_tools.iter().map(|t| t.name.clone()));
        names.sort();
        names.dedup();
        names
    }

    /// How many tools are still unresolved, and so absent from
    /// [`declared_tool_names`](Self::declared_tool_names).
    ///
    /// Non-zero means any name-based check run now is working from a partial
    /// picture. Zero means `declared_tool_names` is the whole set.
    pub fn pending_tool_count(&self) -> usize {
        self.deferred_tools.len()
    }

    /// The governing flow, if [`govern`](Live::govern) or
    /// [`observe`](Live::observe) was called.
    ///
    /// The flow's own `ambient` list is as the caller wrote it; tools registered
    /// through [`ambient_tools`](Live::ambient_tools) are merged in at connect
    /// and are readable separately via
    /// [`ambient_tool_names`](Live::ambient_tools).
    pub fn flow(&self) -> Option<&Flow> {
        self.flow.as_ref()
    }

    /// Whether an attached flow is enforced or merely observed.
    pub fn flow_enforcement(&self) -> Enforcement {
        self.flow_mode
    }

    /// The configured phases, in declaration order.
    pub fn phases(&self) -> &[Phase] {
        &self.phases
    }

    /// The initial phase name, if one was set.
    ///
    /// A phase machine with phases but no initial phase never starts, which is
    /// why this is worth being able to ask about.
    pub fn initial_phase_name(&self) -> Option<&str> {
        self.initial_phase.as_deref()
    }

    /// The state keys under watch, via [`watch`](Live::watch).
    pub fn watched_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.watchers.observed_keys().iter().cloned().collect();
        keys.sort();
        keys
    }

    /// How many turn extractors are installed.
    ///
    /// Extractors are opaque trait objects — this reports presence, which is
    /// enough to tell a session that will populate state from one that will not.
    pub fn extractor_count(&self) -> usize {
        self.extractors.len()
    }

    /// Whether a session persistence backend is attached.
    pub fn has_persistence(&self) -> bool {
        self.persistence.is_some()
    }

    /// The caller-supplied session `State`, if [`with_state`](Live::with_state)
    /// was called.
    pub fn shared_state(&self) -> Option<&State> {
        self.state.as_ref()
    }

    /// How many additive teardown hooks are registered, via
    /// [`on_teardown`](Live::on_teardown).
    ///
    /// Lets an extension assert its own end-of-session wiring: `with_memory`
    /// installs one here, and a session that reports zero will not persist
    /// anything it learned.
    pub fn teardown_hook_count(&self) -> usize {
        self.callbacks.on_teardown.len()
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use gemini_adk_rs::flow::Guard;
    use gemini_adk_rs::llm::{BaseLlm, LlmError, LlmRequest, LlmResponse};
    use serde_json::json;

    use crate::compose::T;
    use crate::live::Live;

    fn sample_tool() -> crate::compose::tools::ToolComposite {
        T::simple("book_table", "Book a table", |_| async {
            Ok(json!({"ok": true}))
        })
    }

    /// Never called: these tests read configuration, they do not run agents.
    struct InertLlm;

    #[async_trait]
    impl BaseLlm for InertLlm {
        fn model_id(&self) -> &str {
            "inert"
        }
        async fn generate(&self, _request: LlmRequest) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Other("inert".into()))
        }
    }

    #[test]
    fn declared_tool_names_sees_dispatcher_and_builtins() {
        let live = Live::builder().with_tools(sample_tool() | T::google_search());
        let names = live.declared_tool_names();
        assert!(names.contains(&"book_table".to_string()), "{names:?}");
        assert!(names.contains(&"google_search".to_string()), "{names:?}");
        assert_eq!(
            live.pending_tool_count(),
            0,
            "nothing here needs an async handshake"
        );
    }

    #[test]
    fn declared_tool_names_covers_agent_tools_before_connect() {
        // Agent tools are built at connect but named up front, so a name-based
        // check must not report them missing.
        let verifier = crate::builder::AgentBuilder::new("verifier")
            .instruction("Verify the caller")
            .build(std::sync::Arc::new(InertLlm));
        let live = Live::builder().agent_tool_arc("verify_identity", "Verify caller", verifier);
        assert!(
            live.declared_tool_names()
                .contains(&"verify_identity".to_string())
        );
    }

    #[test]
    fn flow_and_phases_read_back() {
        let flow = gemini_adk_rs::flow::Flow::new()
            .step("book")
            .allow(["book_table"])
            .done(Guard::called_ok("book_table"))
            .build()
            .expect("valid");
        let live = Live::builder()
            .govern(flow)
            .phase("greet")
            .instruction("Say hello")
            .done()
            .initial_phase("greet");

        assert_eq!(live.flow().map(|f| f.steps.len()), Some(1));
        assert_eq!(
            live.flow_enforcement(),
            gemini_adk_rs::flow::Enforcement::Enforce
        );
        assert_eq!(live.phases().len(), 1);
        assert_eq!(live.initial_phase_name(), Some("greet"));
    }

    #[test]
    fn a_shared_state_is_the_one_the_session_will_run_on() {
        // The gap this closes: tools capture a `State` the caller built, the
        // session ran on a different one, and a `Guard::is_true(..)` reading a
        // key a tool had written never fired — so a governed flow stalled at
        // its first step with every downstream tool refused by a gate whose
        // condition was in fact satisfied.
        let state = gemini_adk_rs::State::new();
        let _ = state.set("identity_verified", true);
        let live = Live::builder().with_state(state.clone());
        assert_eq!(
            live.shared_state()
                .and_then(|s| s.get::<bool>("identity_verified")),
            Some(true),
            "the builder must hold the caller's own state, not a copy or a fresh one"
        );
    }

    #[test]
    fn an_unconfigured_builder_reports_nothing() {
        let live = Live::builder();
        assert!(live.flow().is_none());
        assert!(live.phases().is_empty());
        assert!(live.initial_phase_name().is_none());
        assert!(live.watched_keys().is_empty());
        assert_eq!(live.extractor_count(), 0);
        assert!(!live.has_persistence());
        assert!(live.shared_state().is_none());
    }
}
