//! Integration tests for the gemini-adk-fluent-rs crate.
//! Tests verify the public API surface works correctly across crate boundaries.

use gemini_adk_fluent_rs::prelude::*;
use serde_json::json;

// ── AgentBuilder tests ──────────────────────────────────────────────────────

#[test]
fn agent_builder_basic() {
    let b = AgentBuilder::new("analyst")
        .model(GeminiModel::Gemini2_0FlashLive)
        .instruction("Analyze the given topic");

    assert_eq!(b.name(), "analyst");
    assert_eq!(b.get_model(), Some(&GeminiModel::Gemini2_0FlashLive));
    assert_eq!(b.get_instruction(), Some("Analyze the given topic"));
}

#[test]
fn agent_builder_copy_on_write() {
    let base = AgentBuilder::new("base")
        .instruction("Base instruction")
        .temperature(0.5);

    // Clone and modify -- original must remain unchanged.
    let variant = base
        .clone()
        .temperature(0.9)
        .instruction("Variant instruction");

    assert_eq!(base.get_temperature(), Some(0.5));
    assert_eq!(base.get_instruction(), Some("Base instruction"));
    assert_eq!(variant.get_temperature(), Some(0.9));
    assert_eq!(variant.get_instruction(), Some("Variant instruction"));
}

#[test]
fn agent_builder_with_temperature() {
    let b = AgentBuilder::new("sampler")
        .temperature(0.7)
        .top_p(0.95)
        .top_k(40);

    assert_eq!(b.get_temperature(), Some(0.7));
    assert_eq!(b.get_top_p(), Some(0.95));
    assert_eq!(b.get_top_k(), Some(40));
}

#[test]
fn agent_builder_with_google_search() {
    let b = AgentBuilder::new("searcher").google_search();
    assert_eq!(b.tool_count(), 1);

    // Adding more built-in tools accumulates.
    let b2 = b.code_execution().url_context();
    assert_eq!(b2.tool_count(), 3);
}

#[test]
fn agent_builder_with_thinking() {
    let b = AgentBuilder::new("thinker").thinking(2048);
    assert_eq!(b.get_thinking_budget(), Some(2048));
}

#[test]
fn agent_builder_full_chain() {
    let b = AgentBuilder::new("full")
        .model(GeminiModel::Gemini2_0FlashLive)
        .instruction("Be helpful")
        .temperature(0.3)
        .top_p(0.9)
        .top_k(50)
        .max_output_tokens(4096)
        .thinking(1024)
        .description("A fully configured agent")
        .google_search()
        .writes("output_key")
        .reads("input_key");

    assert_eq!(b.name(), "full");
    assert_eq!(b.get_temperature(), Some(0.3));
    assert_eq!(b.get_max_output_tokens(), Some(4096));
    assert_eq!(b.get_thinking_budget(), Some(1024));
    assert_eq!(b.get_description(), Some("A fully configured agent"));
    assert_eq!(b.get_writes(), &["output_key"]);
    assert_eq!(b.get_reads(), &["input_key"]);
    assert_eq!(b.tool_count(), 1);
}

#[test]
fn agent_builder_text_only_mode() {
    let b = AgentBuilder::new("text").text_only();
    assert!(b.is_text_only());
}

#[test]
fn agent_builder_sub_agents() {
    let child_a = AgentBuilder::new("child_a");
    let child_b = AgentBuilder::new("child_b");
    let parent = AgentBuilder::new("parent")
        .sub_agent(child_a)
        .sub_agent(child_b);

    assert_eq!(parent.get_sub_agents().len(), 2);
    assert_eq!(parent.get_sub_agents()[0].name(), "child_a");
    assert_eq!(parent.get_sub_agents()[1].name(), "child_b");
}

// ── S (State) operator tests ────────────────────────────────────────────────

#[test]
fn state_operators_pick() {
    let mut state = json!({"a": 1, "b": 2, "c": 3});
    S::pick(&["a", "c"]).apply(&mut state);
    assert_eq!(state, json!({"a": 1, "c": 3}));
}

#[test]
fn state_operators_rename() {
    let mut state = json!({"old_key": 42});
    S::rename(&[("old_key", "new_key")]).apply(&mut state);
    assert_eq!(state, json!({"new_key": 42}));
}

#[test]
fn state_operators_set() {
    let mut state = json!({"existing": 1});
    S::set("added", json!("hello")).apply(&mut state);
    assert_eq!(state["added"], "hello");
    assert_eq!(state["existing"], 1);
}

#[test]
fn state_operators_defaults() {
    let mut state = json!({"existing": "yes"});
    S::defaults(json!({"existing": "no", "missing": "added"})).apply(&mut state);
    assert_eq!(state["existing"], "yes"); // not overwritten
    assert_eq!(state["missing"], "added"); // filled in
}

#[test]
fn state_operators_drop() {
    let mut state = json!({"keep": 1, "remove": 2});
    S::drop(&["remove"]).apply(&mut state);
    assert_eq!(state, json!({"keep": 1}));
}

#[test]
fn state_operators_chain() {
    let chain = S::pick(&["a", "b"]) >> S::rename(&[("a", "x")]);
    let mut state = json!({"a": 1, "b": 2, "c": 3});
    chain.apply(&mut state);
    assert_eq!(state, json!({"x": 1, "b": 2}));
}

#[test]
fn state_predicates_is_true() {
    let state = State::new();
    state.set("flag", true);
    let predicate = S::is_true("flag");
    assert!(predicate(&state));
}

#[test]
fn state_predicates_eq() {
    let state = State::new();
    state.set("status", "active");
    let predicate = S::eq("status", "active");
    assert!(predicate(&state));

    let wrong = S::eq("status", "inactive");
    assert!(!wrong(&state));
}

#[test]
fn state_predicates_one_of() {
    let state = State::new();
    state.set("intent", "full_pay");
    let predicate = S::one_of("intent", &["full_pay", "partial_pay"]);
    assert!(predicate(&state));

    state.set("intent", "refuse");
    assert!(!predicate(&state));
}

// ── C (Context) operator tests ──────────────────────────────────────────────

#[test]
fn context_operators_window() {
    let history = vec![Content::user("a"), Content::model("b"), Content::user("c")];
    let result = C::window(2).apply(&history);
    assert_eq!(result.len(), 2);
}

#[test]
fn context_operators_user_only() {
    let history = vec![Content::user("a"), Content::model("b"), Content::user("c")];
    let result = C::user_only().apply(&history);
    assert_eq!(result.len(), 2);
}

#[test]
fn context_operators_model_only() {
    let history = vec![Content::user("a"), Content::model("b"), Content::user("c")];
    let result = C::model_only().apply(&history);
    assert_eq!(result.len(), 1);
}

#[test]
fn context_operators_empty() {
    let history = vec![Content::user("a"), Content::model("b")];
    let result = C::empty().apply(&history);
    assert!(result.is_empty());
}

#[test]
fn context_operators_head() {
    let history = vec![Content::user("a"), Content::model("b"), Content::user("c")];
    let result = C::head(1).apply(&history);
    assert_eq!(result.len(), 1);
}

#[test]
fn context_operators_compose() {
    let chain = C::window(10) + C::user_only() + C::empty();
    assert_eq!(chain.policies.len(), 3);
}

// ── T (Tool) operator tests ────────────────────────────────────────────────

#[test]
fn tool_operators_google_search() {
    let t = T::google_search();
    assert_eq!(t.len(), 1);
}

#[test]
fn tool_operators_code_execution() {
    let t = T::code_execution();
    assert_eq!(t.len(), 1);
}

#[test]
fn tool_operators_url_context() {
    let t = T::url_context();
    assert_eq!(t.len(), 1);
}

#[test]
fn tool_operators_compose_with_bitor() {
    let t = T::google_search() | T::code_execution() | T::url_context();
    assert_eq!(t.len(), 3);
}

#[test]
fn tool_operators_simple() {
    let t = T::simple("greet", "Greets the user", |_args| async {
        Ok(json!({"message": "hello"}))
    });
    assert_eq!(t.len(), 1);
}

// ── P (Prompt) operator tests ───────────────────────────────────────────────

#[test]
fn prompt_operators_role() {
    let s = P::role("analyst");
    assert_eq!(s.render(), "You are analyst.");
}

#[test]
fn prompt_operators_task() {
    let s = P::task("analyze data");
    assert_eq!(s.render(), "Your task: analyze data");
}

#[test]
fn prompt_operators_constraint() {
    let s = P::constraint("be concise");
    assert_eq!(s.render(), "Constraint: be concise");
}

#[test]
fn prompt_operators_format() {
    let s = P::format("JSON");
    assert_eq!(s.render(), "Output format: JSON");
}

#[test]
fn prompt_operators_text() {
    let s = P::text("Free form instruction");
    assert_eq!(s.render(), "Free form instruction");
}

#[test]
fn prompt_operators_context() {
    let s = P::context("user is a developer");
    assert_eq!(s.render(), "Context: user is a developer");
}

#[test]
fn prompt_operators_persona() {
    let s = P::persona("friendly and warm");
    assert_eq!(s.render(), "Persona: friendly and warm");
}

#[test]
fn prompt_operators_guidelines() {
    let s = P::guidelines(&["be concise", "cite sources"]);
    let rendered = s.render();
    assert!(rendered.contains("Guidelines:"));
    assert!(rendered.contains("- be concise"));
    assert!(rendered.contains("- cite sources"));
}

#[test]
fn prompt_composition_with_add() {
    let prompt = P::role("analyst") + P::task("analyze data") + P::format("JSON");
    assert_eq!(prompt.sections.len(), 3);

    let rendered = prompt.render();
    assert!(rendered.contains("You are analyst."));
    assert!(rendered.contains("Your task: analyze data"));
    assert!(rendered.contains("Output format: JSON"));
}

#[test]
fn prompt_into_string() {
    let s: String = P::role("analyst").into();
    assert_eq!(s, "You are analyst.");

    let composite: String = (P::role("analyst") + P::task("research")).into();
    assert!(composite.contains("You are analyst."));
    assert!(composite.contains("Your task: research"));
}

// ── A (Artifact) operator tests ─────────────────────────────────────────────

#[test]
fn artifact_operators_json_output() {
    let comp = A::json_output("report", "Analysis report");
    assert_eq!(comp.len(), 1);
    let outputs = comp.all_outputs();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].mime_type, "application/json");
    assert_eq!(outputs[0].name, "report");
}

#[test]
fn artifact_operators_text_input() {
    let comp = A::text_input("source", "Source document");
    let inputs = comp.all_inputs();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].mime_type, "text/plain");
    assert_eq!(inputs[0].name, "source");
}

#[test]
fn artifact_operators_compose_with_add() {
    let comp = A::json_output("report", "Report")
        + A::text_input("source", "Source")
        + A::json_output("summary", "Summary");
    assert_eq!(comp.len(), 3);
    assert_eq!(comp.all_inputs().len(), 1);
    assert_eq!(comp.all_outputs().len(), 2);
}

// ── Live builder tests ──────────────────────────────────────────────────────

#[test]
fn live_builder_basic_config() {
    // Verify the full builder chain compiles and produces a valid Live instance.
    // Fields are pub(crate), so we verify compilation rather than field access.
    let _live = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .voice(Voice::Kore)
        .instruction("You are a weather assistant")
        .temperature(0.7);
}

#[test]
fn live_builder_with_greeting() {
    // Verify greeting builder method chains correctly.
    let _live = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .greeting("Hello! How can I help you today?");
}

#[test]
fn live_builder_with_transcription() {
    // Verify the transcription builder method compiles and chains.
    let _live = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .transcription(true, true)
        .instruction("Transcribe everything");
}

#[test]
fn live_builder_with_thinking() {
    let _live = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .thinking(1024)
        .include_thoughts()
        .on_thought(|_text| {});
}

#[test]
fn live_builder_with_callbacks() {
    let _live = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .on_audio(|_data| {})
        .on_text(|_text| {})
        .on_vad_start(|| {})
        .on_interrupted(|| async {})
        .on_turn_complete(|| async {});
}

#[test]
fn live_builder_with_google_search() {
    let _live = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .google_search()
        .code_execution();
}

#[test]
fn live_builder_with_phases() {
    // Verify phase builder chain compiles with transitions and terminal phases.
    let _live = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .phase("greeting")
        .instruction("Welcome the user")
        .transition("main", S::is_true("greeted"))
        .done()
        .phase("main")
        .instruction("Help the user")
        .terminal()
        .done()
        .initial_phase("greeting");
}

#[test]
fn live_builder_with_background_tools() {
    let _live = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .tool_background("slow_search")
        .tool_background_with_scheduling("log_event", FunctionResponseScheduling::Silent);
}

#[test]
fn live_builder_with_steering_mode() {
    // Verify steering mode builder method chains correctly.
    let _live = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .steering_mode(SteeringMode::ContextInjection);
}

// ── State tests (through prelude re-export) ─────────────────────────────────

#[test]
fn state_basic_operations() {
    let state = State::new();

    state.set("name", "Alice");
    assert_eq!(state.get::<String>("name"), Some("Alice".to_string()));

    state.set("count", 0u32);
    let new_count = state.modify("count", 0u32, |n| n + 1);
    assert_eq!(new_count, 1);
}

#[test]
fn state_prefixed_scopes() {
    let state = State::new();

    state.app().set("flag", true);
    assert_eq!(state.app().get::<bool>("flag"), Some(true));

    state.user().set("name", "Bob");
    assert_eq!(state.user().get::<String>("name"), Some("Bob".to_string()));

    state.session().set("turn_count", 5u32);
    assert_eq!(state.session().get::<u32>("turn_count"), Some(5));

    state.turn().set("transcript", "hello");
    assert_eq!(
        state.turn().get::<String>("transcript"),
        Some("hello".to_string())
    );
}

#[test]
fn state_derived_fallback() {
    let state = State::new();

    // Setting a derived key
    state.set("derived:risk", 0.85f64);

    // Auto-fallback: state.get("risk") checks "derived:risk"
    assert_eq!(state.get::<f64>("risk"), Some(0.85));
}

#[test]
fn state_with_zero_copy_borrow() {
    let state = State::new();
    state.set("name", "Alice");

    let len = state.with("name", |v| v.as_str().unwrap().len());
    assert_eq!(len, Some(5));
}
