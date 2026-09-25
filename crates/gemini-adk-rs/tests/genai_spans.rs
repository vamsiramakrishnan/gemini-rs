//! A text-agent run emits the GenAI semantic-convention spans.
//!
//! This lives in its own test binary, with a *global* subscriber, on purpose.
//! As a unit test it used a thread-local subscriber (`set_default`), and
//! tracing's callsite cache is process-wide: another test on a parallel thread
//! registering the `chat` callsite while this one installed its subscriber
//! could cache "never enabled" from a stale snapshot, and the run recorded no
//! model-call spans (CI saw 0 of 2). One test per process has no such race.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};

use gemini_adk_rs::State;
use gemini_adk_rs::llm::{LlmResponse, MockLlm};
use gemini_adk_rs::text::{LlmTextAgent, RunRequest, TextAgent};
use gemini_adk_rs::tool::{SimpleTool, ToolDispatcher};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

type Spans = Arc<Mutex<Vec<(String, BTreeMap<String, String>)>>>;

/// Records every span's name and fields, in creation order.
struct Capture(Spans);

struct Fields<'a>(&'a mut BTreeMap<String, String>);

impl Visit for Fields<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.0
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        let mut fields = BTreeMap::new();
        attrs.record(&mut Fields(&mut fields));
        let mut spans = self.0.lock().unwrap();
        spans.push((attrs.metadata().name().to_string(), fields));
        Id::from_u64(spans.len() as u64)
    }
    fn record(&self, id: &Id, values: &Record<'_>) {
        let mut spans = self.0.lock().unwrap();
        let index = usize::try_from(id.into_u64()).unwrap() - 1;
        values.record(&mut Fields(&mut spans[index].1));
    }
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, _: &Event<'_>) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

/// A run exports the GenAI semantic-convention spans an OpenTelemetry
/// backend reads: the agent, each model call with its settings and token
/// usage, and each tool call.
#[tokio::test]
async fn a_run_emits_genai_spans() {
    let spans: Spans = Arc::default();
    tracing::subscriber::set_global_default(Capture(spans.clone()))
        .expect("the only subscriber in this test binary");

    let llm = MockLlm::script([
        LlmResponse::tool_call("get_weather", serde_json::json!({})).with_usage(10, 2),
        LlmResponse::from_text("Cold.").with_usage(20, 1),
    ])
    .with_model_id("gemini-test");
    let mut dispatcher = ToolDispatcher::new();
    dispatcher.register_function(Arc::new(SimpleTool::new(
        "get_weather",
        "Get weather",
        None,
        |_| async { Ok(serde_json::json!({})) },
    )));
    let agent = LlmTextAgent::new("weather", llm)
        .temperature(0.5)
        .tools(Arc::new(dispatcher));
    agent
        .run_with(RunRequest::new("Weather?"), &State::new())
        .await
        .unwrap();

    let spans = spans.lock().unwrap();
    let named = |name: &str| -> Vec<&BTreeMap<String, String>> {
        spans
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, f)| f)
            .collect()
    };

    let agent_span = named("invoke_agent");
    assert_eq!(agent_span.len(), 1);
    assert_eq!(agent_span[0]["otel.name"], "invoke_agent weather");
    assert_eq!(agent_span[0]["gen_ai.agent.name"], "weather");

    let chats = named("chat");
    assert_eq!(chats.len(), 2, "one span per model call");
    assert_eq!(chats[0]["otel.name"], "chat gemini-test");
    assert_eq!(chats[0]["gen_ai.request.model"], "gemini-test");
    assert_eq!(chats[0]["gen_ai.request.temperature"], "0.5");
    assert_eq!(chats[0]["gen_ai.usage.input_tokens"], "10");
    assert_eq!(chats[1]["gen_ai.usage.output_tokens"], "1");
    assert_eq!(chats[1]["gen_ai.response.finish_reasons"], "STOP");

    let tools = named("execute_tool");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["gen_ai.tool.name"], "get_weather");
    assert!(!tools[0].contains_key("error.type"));
}
