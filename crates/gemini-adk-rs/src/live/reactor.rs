//! First-class reactor vocabulary for Live sessions.
//!
//! This module is intentionally small: it defines the normalized events,
//! reactions, and typed effects that existing mechanisms can migrate onto
//! incrementally without rewriting the current control plane in one step.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use gemini_genai_rs::prelude::Content;

use crate::state::StateMutation;

use super::events::LiveEvent;

/// A normalized event that can drive ADK-level reactions.
#[derive(Debug, Clone)]
pub enum ReactorEvent {
    /// Existing semantic Live event.
    Live(LiveEvent),
    /// State changed since the last reactor cursor.
    StateChanged(Vec<StateMutation>),
    /// Periodic tick for timers and sustained conditions.
    TimerTick {
        /// Time observed by the control lane for this tick.
        now: Instant,
    },
    /// Client-side playback drained after model audio was generated.
    PlaybackDrained {
        /// Whether the control plane has armed a deferred model prompt.
        prompt_pending: bool,
    },
    /// Client detected that the user started speaking.
    UserSpeechStarted,
    /// Client detected that the user stopped speaking.
    UserSpeechEnded {
        /// Whether the control plane has armed a deferred model prompt.
        prompt_pending: bool,
    },
    /// User speech ended, but the model may or may not produce a turn.
    SoftTurnComplete,
}

/// Voice-flow state owned by the reactor.
#[derive(Debug, Clone, Default)]
pub struct VoiceRuntimeState {
    /// Whether the client believes the user is currently speaking.
    pub user_speaking: bool,
    /// Whether browser playback is believed to be active.
    pub playback_active: bool,
    /// Whether a deferred model prompt is armed.
    pub prompt_pending: bool,
    /// Monotonic epoch bumped whenever a prompt is cancelled or armed state changes.
    pub prompt_epoch: u64,
    /// Last time the client reported barge-in/user speech start.
    pub last_barge_in_at: Option<Instant>,
    /// Last time browser playback reported drained.
    pub last_playback_drained_at: Option<Instant>,
}

impl VoiceRuntimeState {
    /// Apply an incoming event to the voice runtime state before rules run.
    pub fn apply_event(&mut self, event: &ReactorEvent) {
        match event {
            ReactorEvent::PlaybackDrained { prompt_pending } => {
                self.playback_active = false;
                self.prompt_pending = *prompt_pending;
                self.last_playback_drained_at = Some(Instant::now());
            }
            ReactorEvent::UserSpeechStarted => {
                self.user_speaking = true;
                self.playback_active = false;
                if self.prompt_pending {
                    self.prompt_epoch = self.prompt_epoch.saturating_add(1);
                }
                self.prompt_pending = false;
                self.last_barge_in_at = Some(Instant::now());
            }
            ReactorEvent::UserSpeechEnded { prompt_pending } => {
                self.user_speaking = false;
                self.prompt_pending = *prompt_pending && !self.playback_active;
            }
            _ => {}
        }
    }
}

/// Execution policy for an effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectPolicy {
    /// Whether this effect blocks later effects from running.
    pub mode: EffectMode,
    /// Optional maximum time budget for the effect.
    pub timeout: Option<Duration>,
}

impl Default for EffectPolicy {
    fn default() -> Self {
        Self {
            mode: EffectMode::Blocking,
            timeout: None,
        }
    }
}

/// Whether an effect must complete before the reactor continues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectMode {
    /// Await this effect before continuing.
    Blocking,
    /// Run this effect independently of later effects.
    Concurrent,
}

/// A typed runtime effect emitted by a reaction.
#[derive(Debug, Clone)]
pub enum LiveEffect {
    /// No operation; useful for conditional reaction builders.
    Noop,
    /// Add state/context turns to the session.
    SendContext(Vec<Content>),
    /// Ask the model to generate from accumulated context.
    PromptModel,
    /// Cancel a deferred model prompt while leaving queued context intact.
    CancelDeferredPrompt,
    /// Tell the Live API that user speech activity started.
    SignalUserActivityStart,
    /// Tell the Live API that user speech activity ended.
    SignalUserActivityEnd,
    /// Replace or amend the active instruction.
    UpdateInstruction(String),
    /// Emit a semantic event for observers.
    Emit(LiveEvent),
}

/// A policy-wrapped effect.
#[derive(Debug, Clone)]
pub struct Reaction {
    /// Rule or subsystem that produced the reaction.
    pub source: &'static str,
    /// Runtime effect requested by the rule.
    pub effect: LiveEffect,
    /// Execution policy for the effect.
    pub policy: EffectPolicy,
}

impl Reaction {
    /// Create a blocking reaction.
    pub fn blocking(source: &'static str, effect: LiveEffect) -> Self {
        Self {
            source,
            effect,
            policy: EffectPolicy::default(),
        }
    }

    /// Create a concurrent reaction.
    pub fn concurrent(source: &'static str, effect: LiveEffect) -> Self {
        Self {
            source,
            effect,
            policy: EffectPolicy {
                mode: EffectMode::Concurrent,
                ..EffectPolicy::default()
            },
        }
    }
}

/// A rule that reacts to normalized events and emits typed effects.
pub trait ReactorRule: Send + Sync {
    /// Stable rule name for diagnostics and reaction provenance.
    fn name(&self) -> &str;
    /// Produce reactions for a normalized event.
    fn react(&self, event: &ReactorEvent, voice: &VoiceRuntimeState) -> Vec<Reaction>;
}

/// Ordered collection of reactor rules.
pub struct LiveReactor {
    rules: Vec<Box<dyn ReactorRule>>,
    voice: Mutex<VoiceRuntimeState>,
}

impl Default for LiveReactor {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            voice: Mutex::new(VoiceRuntimeState::default()),
        }
    }
}

impl LiveReactor {
    /// Create an empty reactor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a reactor with the default voice-flow rules.
    pub fn voice_defaults() -> Self {
        let mut reactor = Self::new();
        reactor.add_rule(PromptOnPlaybackDrained);
        reactor.add_rule(UserSpeechActivityRule);
        reactor
    }

    /// Add a rule to the end of the ordered rule list.
    pub fn add_rule(&mut self, rule: impl ReactorRule + 'static) {
        self.rules.push(Box::new(rule));
    }

    /// Run all rules against an event and collect reactions in rule order.
    pub fn react(&self, event: &ReactorEvent) -> Vec<Reaction> {
        let voice = {
            let mut voice = self.voice.lock().expect("voice reactor state poisoned");
            voice.apply_event(event);
            voice.clone()
        };

        self.rules
            .iter()
            .flat_map(|rule| rule.react(event, &voice))
            .collect()
    }

    /// Return a snapshot of the current voice runtime state.
    pub fn voice_state(&self) -> VoiceRuntimeState {
        self.voice
            .lock()
            .expect("voice reactor state poisoned")
            .clone()
    }
}

/// Prompt the model when browser playback is fully drained and a prompt is armed.
pub struct PromptOnPlaybackDrained;

impl ReactorRule for PromptOnPlaybackDrained {
    fn name(&self) -> &str {
        "prompt_on_playback_drained"
    }

    fn react(&self, event: &ReactorEvent, voice: &VoiceRuntimeState) -> Vec<Reaction> {
        if matches!(event, ReactorEvent::PlaybackDrained { .. })
            && voice.prompt_pending
            && !voice.user_speaking
            && !voice.playback_active
        {
            vec![Reaction::blocking(
                "prompt_on_playback_drained",
                LiveEffect::PromptModel,
            )]
        } else {
            Vec::new()
        }
    }
}

/// Cancel pending model prompts and signal activity around user speech.
pub struct UserSpeechActivityRule;

impl ReactorRule for UserSpeechActivityRule {
    fn name(&self) -> &str {
        "user_speech_activity"
    }

    fn react(&self, event: &ReactorEvent, voice: &VoiceRuntimeState) -> Vec<Reaction> {
        match event {
            ReactorEvent::UserSpeechStarted => vec![
                Reaction::blocking("user_speech_activity", LiveEffect::CancelDeferredPrompt),
                Reaction::blocking("user_speech_activity", LiveEffect::SignalUserActivityStart),
            ],
            ReactorEvent::UserSpeechEnded { .. } => {
                let mut reactions = vec![Reaction::blocking(
                    "user_speech_activity",
                    LiveEffect::SignalUserActivityEnd,
                )];
                if voice.prompt_pending && !voice.playback_active {
                    reactions.push(Reaction::blocking(
                        "user_speech_activity",
                        LiveEffect::PromptModel,
                    ));
                }
                reactions
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactor_collects_reactions_in_rule_order() {
        let mut reactor = LiveReactor::new();
        reactor.add_rule(PromptOnPlaybackDrained);

        let reactions = reactor.react(&ReactorEvent::PlaybackDrained {
            prompt_pending: true,
        });
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0].source, "prompt_on_playback_drained");
        assert_eq!(reactions[0].policy.mode, EffectMode::Blocking);
        assert!(matches!(reactions[0].effect, LiveEffect::PromptModel));
    }

    #[test]
    fn playback_drained_without_pending_prompt_is_noop() {
        let reactor = LiveReactor::voice_defaults();

        let reactions = reactor.react(&ReactorEvent::PlaybackDrained {
            prompt_pending: false,
        });

        assert!(reactions.is_empty());
    }

    #[test]
    fn user_speech_started_cancels_prompt_and_signals_activity() {
        let reactor = LiveReactor::voice_defaults();

        let prompt_reactions = reactor.react(&ReactorEvent::PlaybackDrained {
            prompt_pending: true,
        });
        assert_eq!(prompt_reactions.len(), 1);

        let reactions = reactor.react(&ReactorEvent::UserSpeechStarted);

        assert_eq!(reactions.len(), 2);
        assert!(matches!(
            reactions[0].effect,
            LiveEffect::CancelDeferredPrompt
        ));
        assert!(matches!(
            reactions[1].effect,
            LiveEffect::SignalUserActivityStart
        ));
        let voice = reactor.voice_state();
        assert!(voice.user_speaking);
        assert!(!voice.prompt_pending);
        assert_eq!(voice.prompt_epoch, 1);
    }

    #[test]
    fn user_speech_ended_signals_activity_end() {
        let reactor = LiveReactor::voice_defaults();

        let reactions = reactor.react(&ReactorEvent::UserSpeechEnded {
            prompt_pending: false,
        });

        assert_eq!(reactions.len(), 1);
        assert!(matches!(
            reactions[0].effect,
            LiveEffect::SignalUserActivityEnd
        ));
        assert!(!reactor.voice_state().user_speaking);
    }

    #[test]
    fn speech_end_prompts_when_playback_already_drained_and_prompt_pending() {
        let reactor = LiveReactor::voice_defaults();

        reactor.react(&ReactorEvent::UserSpeechStarted);
        let drain_reactions = reactor.react(&ReactorEvent::PlaybackDrained {
            prompt_pending: true,
        });
        assert!(drain_reactions.is_empty());

        let reactions = reactor.react(&ReactorEvent::UserSpeechEnded {
            prompt_pending: true,
        });

        assert_eq!(reactions.len(), 2);
        assert!(matches!(
            reactions[0].effect,
            LiveEffect::SignalUserActivityEnd
        ));
        assert!(matches!(reactions[1].effect, LiveEffect::PromptModel));
    }

    #[test]
    fn playback_drained_does_not_prompt_while_user_is_speaking() {
        let reactor = LiveReactor::voice_defaults();

        reactor.react(&ReactorEvent::UserSpeechStarted);
        let reactions = reactor.react(&ReactorEvent::PlaybackDrained {
            prompt_pending: true,
        });

        assert!(reactions.is_empty());
        let voice = reactor.voice_state();
        assert!(voice.user_speaking);
        assert!(voice.prompt_pending);
    }

    #[test]
    fn speech_start_wins_after_pending_prompt_snapshot() {
        let reactor = LiveReactor::voice_defaults();

        reactor.react(&ReactorEvent::PlaybackDrained {
            prompt_pending: true,
        });
        reactor.react(&ReactorEvent::UserSpeechStarted);
        let reactions = reactor.react(&ReactorEvent::PlaybackDrained {
            prompt_pending: false,
        });

        assert!(reactions.is_empty());
        let voice = reactor.voice_state();
        assert!(voice.user_speaking);
        assert!(!voice.prompt_pending);
        assert_eq!(voice.prompt_epoch, 1);
    }
}
