//! Execute typed Live reactor effects against a session writer.

use std::sync::Arc;

use gemini_genai_rs::prelude::SessionPhase;
use gemini_genai_rs::session::{SessionError, SessionWriter};
use tokio::sync::broadcast;

use super::ExecutionMode;
use super::context_writer::PendingContext;
use super::events::LiveEvent;
use super::reactor::{LiveEffect, Reaction};

/// Executes [`LiveEffect`] values emitted by the Live reactor.
#[derive(Clone)]
pub struct LiveEffectExecutor {
    writer: Arc<dyn SessionWriter>,
    pending_context: Option<Arc<PendingContext>>,
    event_tx: broadcast::Sender<LiveEvent>,
}

impl LiveEffectExecutor {
    /// Create an executor backed by a session writer.
    pub fn new(
        writer: Arc<dyn SessionWriter>,
        pending_context: Option<Arc<PendingContext>>,
        event_tx: broadcast::Sender<LiveEvent>,
    ) -> Self {
        Self {
            writer,
            pending_context,
            event_tx,
        }
    }

    /// Execute a list of policy-wrapped reactions.
    pub async fn execute_reactions(&self, reactions: Vec<Reaction>) -> Result<(), SessionError> {
        for reaction in reactions {
            match reaction.policy.mode {
                ExecutionMode::Blocking => {
                    let executor = self.clone();
                    let fut = executor.execute(reaction.effect);
                    if let Some(timeout) = reaction.policy.timeout {
                        tokio::time::timeout(timeout, fut).await.map_err(|_| {
                            SessionError::Timeout {
                                phase: SessionPhase::Active,
                                elapsed: timeout,
                            }
                        })??;
                    } else {
                        fut.await?;
                    }
                }
                ExecutionMode::Concurrent => {
                    let executor = self.clone();
                    let timeout = reaction.policy.timeout;
                    let source = reaction.source;
                    let effect = reaction.effect;
                    tokio::spawn(async move {
                        let result = match timeout {
                            Some(timeout) => {
                                tokio::time::timeout(timeout, executor.execute(effect))
                                    .await
                                    .unwrap_or(Err(SessionError::Timeout {
                                        phase: SessionPhase::Active,
                                        elapsed: timeout,
                                    }))
                            }
                            None => executor.execute(effect).await,
                        };
                        // Supervise: surface concurrent failures rather than
                        // silently dropping them.
                        if let Err(err) = result {
                            let _ = executor.event_tx.send(LiveEvent::Error(format!(
                                "reaction '{source}' failed: {err}"
                            )));
                        }
                    });
                }
            }
        }
        Ok(())
    }

    /// Execute one typed effect.
    pub async fn execute(&self, effect: LiveEffect) -> Result<(), SessionError> {
        match effect {
            LiveEffect::Noop => Ok(()),
            LiveEffect::SendContext(contents) => {
                if !contents.is_empty() {
                    self.writer.send_client_content(contents, false).await?;
                }
                Ok(())
            }
            LiveEffect::PromptModel => self.flush_deferred_prompt().await,
            LiveEffect::CancelDeferredPrompt => {
                if let Some(pending) = &self.pending_context {
                    pending.clear_prompt();
                }
                Ok(())
            }
            LiveEffect::SignalUserActivityStart => self.writer.signal_activity_start().await,
            LiveEffect::SignalUserActivityEnd => self.writer.signal_activity_end().await,
            LiveEffect::UpdateInstruction(instruction) => {
                self.writer.update_instruction(instruction).await
            }
            LiveEffect::Emit(event) => {
                let _ = self.event_tx.send(event);
                Ok(())
            }
        }
    }

    /// Flush deferred context and an armed prompt.
    ///
    /// This is intentionally gated by [`PendingContext::take_prompt`], so a
    /// playback-drained event cannot trigger a new empty model turn unless the
    /// control plane explicitly armed one.
    pub async fn flush_deferred_prompt(&self) -> Result<(), SessionError> {
        let Some(pending) = &self.pending_context else {
            return Ok(());
        };

        let contents = pending.drain_context();
        if !contents.is_empty() {
            self.writer.send_client_content(contents, false).await?;
        }
        if pending.take_prompt() {
            self.writer.send_client_content(vec![], true).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use gemini_genai_rs::prelude::{Content, FunctionResponse};
    use parking_lot::Mutex;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Write {
        ClientContent { turns: usize, turn_complete: bool },
        Instruction(String),
        ActivityStart,
        ActivityEnd,
    }

    #[derive(Default)]
    struct MockWriter {
        writes: Mutex<Vec<Write>>,
    }

    #[async_trait]
    impl SessionWriter for MockWriter {
        async fn send_audio(&self, _data: bytes::Bytes) -> Result<(), SessionError> {
            Ok(())
        }

        async fn send_text(&self, _text: String) -> Result<(), SessionError> {
            Ok(())
        }

        async fn send_tool_response(
            &self,
            _responses: Vec<FunctionResponse>,
        ) -> Result<(), SessionError> {
            Ok(())
        }

        async fn send_client_content(
            &self,
            turns: Vec<Content>,
            turn_complete: bool,
        ) -> Result<(), SessionError> {
            self.writes.lock().push(Write::ClientContent {
                turns: turns.len(),
                turn_complete,
            });
            Ok(())
        }

        async fn send_video(&self, _jpeg_data: bytes::Bytes) -> Result<(), SessionError> {
            Ok(())
        }

        async fn update_instruction(&self, instruction: String) -> Result<(), SessionError> {
            self.writes.lock().push(Write::Instruction(instruction));
            Ok(())
        }

        async fn signal_activity_start(&self) -> Result<(), SessionError> {
            self.writes.lock().push(Write::ActivityStart);
            Ok(())
        }

        async fn signal_activity_end(&self) -> Result<(), SessionError> {
            self.writes.lock().push(Write::ActivityEnd);
            Ok(())
        }

        async fn disconnect(&self) -> Result<(), SessionError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn prompt_model_flushes_context_then_armed_prompt() {
        let writer = Arc::new(MockWriter::default());
        let pending = Arc::new(PendingContext::new());
        pending.push(Content::model("phase context"));
        pending.set_prompt();
        let (event_tx, _) = broadcast::channel(8);
        let executor = LiveEffectExecutor::new(writer.clone(), Some(pending.clone()), event_tx);

        executor.execute(LiveEffect::PromptModel).await.unwrap();

        assert_eq!(
            writer.writes.lock().as_slice(),
            &[
                Write::ClientContent {
                    turns: 1,
                    turn_complete: false
                },
                Write::ClientContent {
                    turns: 0,
                    turn_complete: true
                }
            ]
        );
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn prompt_model_without_armed_prompt_only_flushes_context() {
        let writer = Arc::new(MockWriter::default());
        let pending = Arc::new(PendingContext::new());
        pending.push(Content::model("phase context"));
        let (event_tx, _) = broadcast::channel(8);
        let executor = LiveEffectExecutor::new(writer.clone(), Some(pending), event_tx);

        executor.execute(LiveEffect::PromptModel).await.unwrap();

        assert_eq!(
            writer.writes.lock().as_slice(),
            &[Write::ClientContent {
                turns: 1,
                turn_complete: false
            }]
        );
    }

    #[tokio::test]
    async fn update_instruction_uses_writer() {
        let writer = Arc::new(MockWriter::default());
        let (event_tx, _) = broadcast::channel(8);
        let executor = LiveEffectExecutor::new(writer.clone(), None, event_tx);

        executor
            .execute(LiveEffect::UpdateInstruction("new instruction".into()))
            .await
            .unwrap();

        assert_eq!(
            writer.writes.lock().as_slice(),
            &[Write::Instruction("new instruction".into())]
        );
    }

    #[tokio::test]
    async fn cancel_deferred_prompt_keeps_context() {
        let writer = Arc::new(MockWriter::default());
        let pending = Arc::new(PendingContext::new());
        pending.push(Content::model("still useful with user audio"));
        pending.set_prompt();
        let (event_tx, _) = broadcast::channel(8);
        let executor = LiveEffectExecutor::new(writer, Some(pending.clone()), event_tx);

        executor
            .execute(LiveEffect::CancelDeferredPrompt)
            .await
            .unwrap();

        assert!(!pending.has_prompt());
        assert_eq!(pending.drain_context().len(), 1);
    }

    #[tokio::test]
    async fn user_activity_effects_signal_writer() {
        let writer = Arc::new(MockWriter::default());
        let (event_tx, _) = broadcast::channel(8);
        let executor = LiveEffectExecutor::new(writer.clone(), None, event_tx);

        executor
            .execute_reactions(vec![
                Reaction::blocking("test", LiveEffect::SignalUserActivityStart),
                Reaction::blocking("test", LiveEffect::SignalUserActivityEnd),
            ])
            .await
            .unwrap();

        assert_eq!(
            writer.writes.lock().as_slice(),
            &[Write::ActivityStart, Write::ActivityEnd]
        );
    }

    #[tokio::test]
    async fn concurrent_effect_failure_is_surfaced_as_event() {
        struct FailWriter;
        #[async_trait]
        impl SessionWriter for FailWriter {
            async fn send_audio(&self, _: bytes::Bytes) -> Result<(), SessionError> {
                Ok(())
            }
            async fn send_text(&self, _: String) -> Result<(), SessionError> {
                Ok(())
            }
            async fn send_tool_response(
                &self,
                _: Vec<FunctionResponse>,
            ) -> Result<(), SessionError> {
                Ok(())
            }
            async fn send_client_content(
                &self,
                _: Vec<Content>,
                _: bool,
            ) -> Result<(), SessionError> {
                Err(SessionError::NotConnected)
            }
            async fn send_video(&self, _: bytes::Bytes) -> Result<(), SessionError> {
                Ok(())
            }
            async fn update_instruction(&self, _: String) -> Result<(), SessionError> {
                Ok(())
            }
            async fn signal_activity_start(&self) -> Result<(), SessionError> {
                Ok(())
            }
            async fn signal_activity_end(&self) -> Result<(), SessionError> {
                Ok(())
            }
            async fn disconnect(&self) -> Result<(), SessionError> {
                Ok(())
            }
        }

        let (event_tx, mut rx) = broadcast::channel(8);
        let executor = LiveEffectExecutor::new(Arc::new(FailWriter), None, event_tx);

        // A concurrent effect that fails must surface as a LiveEvent, not vanish.
        executor
            .execute_reactions(vec![Reaction::concurrent(
                "test",
                LiveEffect::SendContext(vec![Content::model("x")]),
            )])
            .await
            .unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("a reaction-failure event within the timeout")
            .expect("event received");
        assert!(
            matches!(&event, LiveEvent::Error(msg) if msg.contains("reaction 'test' failed")),
            "expected a reaction-failure error event, got {event:?}"
        );
    }
}
