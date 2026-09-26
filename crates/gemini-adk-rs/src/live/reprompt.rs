//! Reprompt on silence: the control-lane side of a stage's
//! [`VoiceTiming::reprompt_after_ms`](crate::flow::VoiceTiming).
//!
//! The telemetry lane keeps `session:silence_ms` current (time since the last
//! session event, on the state's clock). When it passes the active stage's
//! reprompt threshold while the model is quiet, the model is told to repeat
//! its question, once per silence: any activity, including the model's
//! reply, ends the silence and re-arms the timer.

use std::sync::Arc;
use std::time::Duration;

use gemini_genai_rs::prelude::Content;
use gemini_genai_rs::session::SessionWriter;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::events::LiveEvent;
use crate::flow::{VOICE_TIMING_KEY, VoiceTiming};
use crate::state::State;

/// How often the timer looks at the silence.
const TICK: Duration = Duration::from_millis(100);

pub(crate) async fn run_reprompt_timer(
    state: State,
    writer: Arc<dyn SessionWriter>,
    events: broadcast::Sender<LiveEvent>,
    cancel: CancellationToken,
) {
    let mut tick = tokio::time::interval(TICK);
    let mut fired = false;
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = tick.tick() => {}
        }
        let Some(after) = state
            .get::<VoiceTiming>(VOICE_TIMING_KEY)
            .and_then(|t| t.reprompt_after_ms)
        else {
            fired = false;
            continue;
        };
        let silence_ms = state.session().get::<u64>("silence_ms").unwrap_or(0);
        if silence_ms < after {
            fired = false;
            continue;
        }
        let model_speaking = state
            .session()
            .get::<bool>("is_model_speaking")
            .unwrap_or(false);
        if fired || model_speaking {
            continue;
        }
        fired = true;
        let text = state
            .get::<VoiceTiming>(VOICE_TIMING_KEY)
            .map(|t| t.reprompt_text().to_string())
            .unwrap_or_default();
        if writer
            .send_client_content(vec![Content::user(text)], true)
            .await
            .is_ok()
        {
            let _ = events.send(LiveEvent::Reprompted { silence_ms });
        }
    }
}
