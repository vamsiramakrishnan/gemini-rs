//! A Live session is one trace: a `live_session` root span carrying the
//! conversation id, with every turn span beneath it.

use std::sync::{Arc, Mutex};

use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::testing::ScriptedServer;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

#[derive(Debug, Clone)]
struct Seen {
    name: String,
    parent: Option<String>,
    conversation_id: Option<String>,
}

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Seen>>>);

struct ConversationId(Option<String>);

impl Visit for ConversationId {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "gen_ai.conversation.id" {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for Recorder
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let span = ctx.span(id).expect("new span is registered");
        let mut conversation = ConversationId(None);
        attrs.record(&mut conversation);
        self.0.lock().unwrap().push(Seen {
            name: span.name().to_string(),
            parent: span.parent().map(|p| p.name().to_string()),
            conversation_id: conversation.0,
        });
    }
}

#[tokio::test]
async fn every_turn_span_belongs_to_the_session_span() {
    let recorder = Recorder::default();
    let subscriber = tracing_subscriber::registry().with(recorder.clone());
    let _default = tracing::subscriber::set_default(subscriber);

    let run = ScriptedServer::new()
        .says("Hello.")
        .says("Goodbye.")
        .play(Live::builder().session_id("call-42"))
        .await
        .unwrap();
    run.disconnect().await;

    let seen = recorder.0.lock().unwrap().clone();
    let session: Vec<_> = seen.iter().filter(|s| s.name == "live_session").collect();
    assert_eq!(session.len(), 1, "{seen:?}");
    assert_eq!(session[0].conversation_id.as_deref(), Some("call-42"));
    let turns: Vec<_> = seen.iter().filter(|s| s.name == "turn").collect();
    assert!(turns.len() >= 2, "{seen:?}");
    assert!(
        turns
            .iter()
            .all(|t| t.parent.as_deref() == Some("live_session")),
        "{turns:?}"
    );
}
