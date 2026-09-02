//! Warm handoff — transfer the conversation to a human with its context
//! intact.
//!
//! The single UX bar for an escalation is that the person picking up never
//! asks the caller to repeat themselves. What the receiving desk needs is
//! not the raw session but a compact, serializable packet: who is calling,
//! what has been said, what the governed flow has established, and — when a
//! summarizer is available — two sentences of "what they want and what was
//! already tried".
//!
//! [`HandoffRecorder`] accumulates the final transcripts as the call runs
//! (redacted upstream when
//! [`redaction`](gemini_adk_rs::live::redaction) is installed, so a packet
//! can never leak what the router already scrubbed). [`HandoffPacket`] is
//! the snapshot the connector delivers however its platform wants —
//! a screen-pop payload, SIP headers, a CRM note. Assembly is transport-
//! agnostic by design; delivering it is the connector's job.
//!
//! ```no_run
//! # use gemini_adk_fluent_rs::prelude::*;
//! # use gemini_adk_fluent_rs::handoff::HandoffRecorder;
//! # use std::sync::Arc;
//! # async fn run(handle: LiveHandle, flash_llm: Arc<dyn BaseLlm>) -> Result<(), Box<dyn std::error::Error>> {
//! let recorder = HandoffRecorder::attach(&handle, 40);
//! // … the call runs; escalation triggers …
//! let mut packet = recorder.packet(&handle, &["telephony:caller", "intent", "verified"]);
//! packet.summarize(&*flash_llm).await.ok();  // optional 2–3 sentence summary
//! let json = serde_json::to_string(&packet)?;  // hand this to the agent desktop
//! # let _ = json; Ok(())
//! # }
//! ```

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use gemini_adk_rs::live::{LiveEvent, LiveHandle};
use gemini_adk_rs::llm::{BaseLlm, LlmError, LlmRequest};

/// One finalized turn of the conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HandoffTurn {
    /// `"caller"` or `"agent"`.
    pub speaker: String,
    /// The final transcript of the turn (redacted upstream if redaction is
    /// installed on the session).
    pub text: String,
}

/// The context packet handed to the receiving human.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HandoffPacket {
    /// A short synthesized summary of what the caller wants and what has
    /// been attempted — filled by [`summarize`](Self::summarize), `None`
    /// until then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The last N finalized turns, oldest first.
    pub transcript: Vec<HandoffTurn>,
    /// The requested state keys and their current values — authentication
    /// status, captured intent, caller identity, whatever the deployment
    /// selects. Keys absent from state are omitted.
    pub state: BTreeMap<String, serde_json::Value>,
    /// The governed flow's standing at handoff: steps done, steps active,
    /// and requirements still unmet. `None` when the session is ungoverned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow: Option<HandoffFlowStatus>,
}

/// The flow's standing at the moment of handoff.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HandoffFlowStatus {
    /// Steps that have latched done.
    pub done: Vec<String>,
    /// Steps currently active.
    pub active: Vec<String>,
    /// Required steps not yet completed — what the human still has to do.
    pub missing: Vec<String>,
}

impl HandoffPacket {
    /// Fill [`summary`](Self::summary) with a 2–3 sentence synthesis of the
    /// transcript, using any [`BaseLlm`]. The packet is useful without it;
    /// call this only when the escalation path has the latency budget.
    pub async fn summarize(&mut self, llm: &dyn BaseLlm) -> Result<(), LlmError> {
        let mut conversation = String::new();
        for turn in &self.transcript {
            conversation.push_str(&format!("{}: {}\n", turn.speaker, turn.text));
        }
        let mut request = LlmRequest::from_text(conversation);
        request.system_instruction = Some(
            "Summarize this call for the human agent about to take it over, \
             in 2-3 sentences: what the caller wants, and what has already \
             been tried or established. No preamble."
                .into(),
        );
        self.summary = Some(llm.generate(request).await?.text());
        Ok(())
    }
}

/// Accumulates the conversation as it happens, so a packet can be assembled
/// at any moment without asking the caller to wait.
pub struct HandoffRecorder {
    turns: Arc<parking_lot::Mutex<VecDeque<HandoffTurn>>>,
    task: JoinHandle<()>,
}

impl HandoffRecorder {
    /// Start recording finalized transcripts from the session's event
    /// stream, keeping the most recent `max_turns`.
    pub fn attach(handle: &LiveHandle, max_turns: usize) -> HandoffRecorder {
        let turns: Arc<parking_lot::Mutex<VecDeque<HandoffTurn>>> =
            Arc::new(parking_lot::Mutex::new(VecDeque::new()));
        let mut events = handle.events();
        let store = turns.clone();
        let task = tokio::spawn(async move {
            loop {
                let (speaker, text) = match events.recv().await {
                    Ok(LiveEvent::InputTranscript {
                        text,
                        is_final: true,
                    }) => ("caller", text),
                    Ok(LiveEvent::OutputTranscript {
                        text,
                        is_final: true,
                    }) => ("agent", text),
                    Ok(LiveEvent::TextComplete(text)) => ("agent", text),
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if text.trim().is_empty() {
                    continue;
                }
                let mut turns = store.lock();
                turns.push_back(HandoffTurn {
                    speaker: speaker.into(),
                    text,
                });
                while turns.len() > max_turns {
                    turns.pop_front();
                }
            }
        });
        HandoffRecorder { turns, task }
    }

    /// Assemble the packet: recorded transcript, the requested state keys,
    /// and the flow's current standing. Synchronous — callable from any
    /// escalation path, including a tool handler.
    pub fn packet(&self, handle: &LiveHandle, state_keys: &[&str]) -> HandoffPacket {
        let state = handle.state();
        let mut selected = BTreeMap::new();
        for &key in state_keys {
            if let Some(value) = state.get::<serde_json::Value>(key) {
                selected.insert(key.to_string(), value);
            }
        }
        let flow = handle.explain().map(|explanation| HandoffFlowStatus {
            done: state.get::<Vec<String>>("flow:done").unwrap_or_default(),
            active: explanation.active,
            missing: explanation.missing_requirements,
        });
        HandoffPacket {
            summary: None,
            transcript: self.turns.lock().iter().cloned().collect(),
            state: selected,
            flow,
        }
    }

    /// Stop recording. Packets already assembled are unaffected.
    pub fn detach(self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_serializes_without_optional_sections() {
        let packet = HandoffPacket {
            summary: None,
            transcript: vec![HandoffTurn {
                speaker: "caller".into(),
                text: "I want to change my booking".into(),
            }],
            state: BTreeMap::from([("verified".into(), serde_json::json!(true))]),
            flow: None,
        };
        let json = serde_json::to_value(&packet).unwrap();
        assert!(json.get("summary").is_none(), "None summary is omitted");
        assert!(json.get("flow").is_none(), "ungoverned session omits flow");
        assert_eq!(json["transcript"][0]["speaker"], "caller");
        assert_eq!(json["state"]["verified"], true);
    }
}
