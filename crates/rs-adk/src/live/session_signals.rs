//! Auto-tracked session-level state signals.
//!
//! [`SessionSignals`] is called by the telemetry lane on every
//! [`SessionEvent`] and transparently updates keys under the `session:`
//! prefix in the shared [`State`].
//!
//! Hot-path timestamps use [`AtomicU64`] (nanos since start) instead of
//! `Mutex<Instant>`, eliminating per-event mutex contention. Derived
//! timing signals (`silence_ms`, `elapsed_ms`, `remaining_budget_ms`)
//! are flushed periodically via `flush_timing()` rather than on every event.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;
use rs_genai::prelude::{SessionEvent, SessionPhase};

use crate::state::State;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Session type determines the server-side duration limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionType {
    /// Audio-only session (~15 min limit).
    AudioOnly,
    /// Audio + video session (~2 min limit).
    AudioVideo,
}

// ---------------------------------------------------------------------------
// SessionSignals
// ---------------------------------------------------------------------------

/// Tracks session-level signals automatically from events.
///
/// Every call to [`on_event`](SessionSignals::on_event) updates the
/// corresponding keys under `session:` in the shared [`State`], making
/// them available to instruction templates, watchers, and computed vars.
///
/// **Performance**: Timestamps use `AtomicU64` (nanos since session start)
/// instead of `Mutex<Instant>`. Derived timing signals are flushed
/// periodically via `flush_timing()` (100ms interval) rather than per-event.
pub struct SessionSignals {
    state: State,
    /// Session start time — used as epoch for all atomic timestamps.
    start: Instant,
    /// Nanos since start when connected.
    connected_at_ns: AtomicU64,
    /// Whether currently connected.
    is_connected: AtomicBool,
    /// Nanos since start of last activity.
    last_activity_ns: AtomicU64,
    /// Whether the session includes video input.
    has_video: AtomicBool,
    /// Server-sent GoAway timestamp, if received.
    go_away_at: Mutex<Option<Instant>>,
    /// Latest resumption handle from server (persisted for reconnection).
    latest_resume_handle: Mutex<Option<String>>,
}

impl SessionSignals {
    /// Create a new `SessionSignals` backed by the given [`State`].
    pub fn new(state: State) -> Self {
        Self {
            state,
            start: Instant::now(),
            connected_at_ns: AtomicU64::new(0),
            is_connected: AtomicBool::new(false),
            last_activity_ns: AtomicU64::new(0),
            has_video: AtomicBool::new(false),
            go_away_at: Mutex::new(None),
            latest_resume_handle: Mutex::new(None),
        }
    }

    /// Process an event — updates state keys and atomic timestamps.
    ///
    /// This is the per-event handler. It updates boolean flags, counters,
    /// and atomic timestamps. **Derived timing** (silence_ms, elapsed_ms,
    /// remaining_budget_ms) is NOT computed here — call `flush_timing()`
    /// periodically instead.
    pub fn on_event(&self, event: &SessionEvent) {
        match event {
            SessionEvent::Connected => {
                let now_ns = self.elapsed_ns();
                self.connected_at_ns.store(now_ns, Ordering::Relaxed);
                self.is_connected.store(true, Ordering::Relaxed);
                self.last_activity_ns.store(now_ns, Ordering::Relaxed);
                self.state.session().set("connected_at_ms", 0u64);
                self.state.session().set("interrupt_count", 0u64);
                self.state.session().set("error_count", 0u64);
                self.state.session().set("is_user_speaking", false);
                self.state.session().set("is_model_speaking", false);
                self.state.session().set("go_away_received", false);
                self.state.session().set("resumable", false);
                self.state.session().set("session_type", "audio_only");
            }

            SessionEvent::VoiceActivityStart => {
                self.state.session().set("is_user_speaking", true);
                self.touch_activity();
            }

            SessionEvent::VoiceActivityEnd => {
                self.state.session().set("is_user_speaking", false);
                self.touch_activity();
            }

            SessionEvent::Interrupted => {
                let count: u64 = self.state.session().get("interrupt_count").unwrap_or(0);
                self.state.session().set("interrupt_count", count + 1);
                self.touch_activity();
            }

            SessionEvent::Error(msg) => {
                let count: u64 = self.state.session().get("error_count").unwrap_or(0);
                self.state.session().set("error_count", count + 1);
                self.state.session().set("last_error", msg.clone());
            }

            SessionEvent::PhaseChanged(phase) => {
                self.state
                    .session()
                    .set("is_model_speaking", *phase == SessionPhase::ModelSpeaking);
                self.state.session().set("phase", phase.to_string());
                self.touch_activity();
            }

            SessionEvent::GoAway(time_left) => {
                self.state.session().set("go_away_received", true);
                if let Some(ref tl) = time_left {
                    self.state.session().set("go_away_time_left", tl.clone());
                    if let Ok(secs) = tl.trim_end_matches('s').parse::<u64>() {
                        let deadline = Instant::now() + std::time::Duration::from_secs(secs);
                        *self.go_away_at.lock() = Some(deadline);
                        self.state
                            .session()
                            .set("go_away_time_left_ms", secs * 1000);
                    }
                }
            }

            SessionEvent::SessionResumeUpdate(info) => {
                *self.latest_resume_handle.lock() = Some(info.handle.clone());
                self.state.session().set("resumable", info.resumable);
                if let Some(ref idx) = info.last_consumed_index {
                    self.state
                        .session()
                        .set("last_consumed_client_index", idx.clone());
                }
            }

            SessionEvent::Usage(usage) => {
                if let Some(total) = usage.total_token_count {
                    self.state.session().set("total_token_count", total);
                }
                if let Some(prompt) = usage.prompt_token_count {
                    self.state.session().set("prompt_token_count", prompt);
                }
                if let Some(response) = usage.response_token_count {
                    self.state.session().set("response_token_count", response);
                }
                if let Some(cached) = usage.cached_content_token_count {
                    self.state
                        .session()
                        .set("cached_content_token_count", cached);
                }
                if let Some(thoughts) = usage.thoughts_token_count {
                    self.state.session().set("thoughts_token_count", thoughts);
                }
            }

            SessionEvent::GenerationComplete => {
                // No-op for signals — generation complete is handled by control lane
            }

            SessionEvent::InputTranscription(text) => {
                self.state
                    .session()
                    .set("last_input_transcription", text.clone());
                self.touch_activity();
            }

            SessionEvent::OutputTranscription(text) => {
                self.state
                    .session()
                    .set("last_output_transcription", text.clone());
                self.touch_activity();
            }

            SessionEvent::AudioData(_)
            | SessionEvent::TextDelta(_)
            | SessionEvent::TextComplete(_) => {
                // High-frequency events: only touch the atomic timestamp.
                // No DashMap writes, no mutex locks.
                self.touch_activity();
            }

            SessionEvent::TurnComplete => {
                self.touch_activity();
            }

            SessionEvent::Disconnected(_reason) => {
                self.is_connected.store(false, Ordering::Relaxed);
                self.state.session().set("disconnected", true);
            }

            _ => {}
        }
    }

    /// Flush derived timing signals to state.
    ///
    /// Call this periodically (e.g., every 100ms) from the telemetry lane.
    /// Computes `silence_ms`, `elapsed_ms`, and `remaining_budget_ms` from
    /// atomic timestamps without any mutex locks.
    pub fn flush_timing(&self) {
        let last_activity = self.last_activity_ns.load(Ordering::Relaxed);
        if last_activity > 0 {
            let now_ns = self.elapsed_ns();
            let silence_ms = now_ns.saturating_sub(last_activity) / 1_000_000;
            self.state.session().set("silence_ms", silence_ms);
        }

        if self.is_connected.load(Ordering::Relaxed) {
            let connected_ns = self.connected_at_ns.load(Ordering::Relaxed);
            let now_ns = self.elapsed_ns();
            let elapsed_ms = now_ns.saturating_sub(connected_ns) / 1_000_000;
            self.state.session().set("elapsed_ms", elapsed_ms);

            let limit_ms: u64 = match self.session_type() {
                SessionType::AudioOnly => 15 * 60 * 1000,
                SessionType::AudioVideo => 2 * 60 * 1000,
            };
            let remaining = limit_ms.saturating_sub(elapsed_ms);
            self.state.session().set("remaining_budget_ms", remaining);
        }
    }

    #[inline]
    fn touch_activity(&self) {
        self.last_activity_ns
            .store(self.elapsed_ns(), Ordering::Relaxed);
    }

    #[inline]
    fn elapsed_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }

    /// Returns the current session type based on whether video has been sent.
    pub fn session_type(&self) -> SessionType {
        if self.has_video.load(Ordering::Relaxed) {
            SessionType::AudioVideo
        } else {
            SessionType::AudioOnly
        }
    }

    /// Returns the latest resumption handle for reconnection.
    pub fn latest_resume_handle(&self) -> Option<String> {
        self.latest_resume_handle.lock().clone()
    }

    /// Mark that video has been sent (changes session type to `AudioVideo`).
    pub fn mark_video_sent(&self) {
        if !self.has_video.swap(true, Ordering::Relaxed) {
            self.state.session().set("session_type", "audio_video");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use rs_genai::prelude::SessionEvent;

    fn signals() -> SessionSignals {
        SessionSignals::new(State::new())
    }

    #[test]
    fn connected_initializes_state() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);

        assert_eq!(s.state.session().get::<u64>("connected_at_ms"), Some(0));
        assert_eq!(s.state.session().get::<u64>("interrupt_count"), Some(0));
        assert_eq!(s.state.session().get::<u64>("error_count"), Some(0));
        assert_eq!(
            s.state.session().get::<bool>("is_user_speaking"),
            Some(false)
        );
        assert_eq!(
            s.state.session().get::<bool>("is_model_speaking"),
            Some(false)
        );
        assert_eq!(
            s.state.session().get::<bool>("go_away_received"),
            Some(false)
        );
        assert_eq!(s.state.session().get::<bool>("resumable"), Some(false));
        assert_eq!(
            s.state.session().get::<String>("session_type"),
            Some("audio_only".to_string())
        );
        assert!(s.is_connected.load(Ordering::Relaxed));
    }

    #[test]
    fn voice_activity_toggles_user_speaking() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::VoiceActivityStart);
        assert_eq!(
            s.state.session().get::<bool>("is_user_speaking"),
            Some(true)
        );
        s.on_event(&SessionEvent::VoiceActivityEnd);
        assert_eq!(
            s.state.session().get::<bool>("is_user_speaking"),
            Some(false)
        );
    }

    #[test]
    fn interrupted_increments_count() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::Interrupted);
        assert_eq!(s.state.session().get::<u64>("interrupt_count"), Some(1));
        s.on_event(&SessionEvent::Interrupted);
        assert_eq!(s.state.session().get::<u64>("interrupt_count"), Some(2));
        s.on_event(&SessionEvent::Interrupted);
        assert_eq!(s.state.session().get::<u64>("interrupt_count"), Some(3));
    }

    #[test]
    fn error_increments_count() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::Error("oops".into()));
        assert_eq!(s.state.session().get::<u64>("error_count"), Some(1));
        assert_eq!(
            s.state.session().get::<String>("last_error"),
            Some("oops".into())
        );
        s.on_event(&SessionEvent::Error("oops2".into()));
        assert_eq!(s.state.session().get::<u64>("error_count"), Some(2));
        assert_eq!(
            s.state.session().get::<String>("last_error"),
            Some("oops2".into())
        );
    }

    #[test]
    fn phase_changed_sets_model_speaking() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::PhaseChanged(SessionPhase::ModelSpeaking));
        assert_eq!(
            s.state.session().get::<bool>("is_model_speaking"),
            Some(true)
        );
        assert_eq!(
            s.state.session().get::<String>("phase"),
            Some("ModelSpeaking".into())
        );
        s.on_event(&SessionEvent::PhaseChanged(SessionPhase::Active));
        assert_eq!(
            s.state.session().get::<bool>("is_model_speaking"),
            Some(false)
        );
        assert_eq!(
            s.state.session().get::<String>("phase"),
            Some("Active".into())
        );
    }

    #[test]
    fn go_away_sets_state() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::GoAway(Some("60s".into())));
        assert_eq!(
            s.state.session().get::<bool>("go_away_received"),
            Some(true)
        );
        assert_eq!(
            s.state.session().get::<String>("go_away_time_left"),
            Some("60s".into())
        );
        assert_eq!(
            s.state.session().get::<u64>("go_away_time_left_ms"),
            Some(60_000)
        );
        assert!(s.go_away_at.lock().is_some());
    }

    #[test]
    fn go_away_without_time_left() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::GoAway(None));
        assert_eq!(
            s.state.session().get::<bool>("go_away_received"),
            Some(true)
        );
        assert_eq!(s.state.session().get::<String>("go_away_time_left"), None);
        assert!(s.go_away_at.lock().is_none());
    }

    #[test]
    fn session_resume_handle_stored() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::SessionResumeUpdate(
            rs_genai::session::ResumeInfo {
                handle: "handle-abc".into(),
                resumable: true,
                last_consumed_index: None,
            },
        ));
        assert_eq!(s.state.session().get::<bool>("resumable"), Some(true));
        assert_eq!(s.latest_resume_handle(), Some("handle-abc".to_string()));
    }

    #[test]
    fn transcription_stores_last() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::InputTranscription("hello".into()));
        assert_eq!(
            s.state.session().get::<String>("last_input_transcription"),
            Some("hello".into())
        );
        s.on_event(&SessionEvent::OutputTranscription("hi there".into()));
        assert_eq!(
            s.state.session().get::<String>("last_output_transcription"),
            Some("hi there".into())
        );
        s.on_event(&SessionEvent::InputTranscription("bye".into()));
        assert_eq!(
            s.state.session().get::<String>("last_input_transcription"),
            Some("bye".into())
        );
    }

    #[test]
    fn session_type_defaults_to_audio_only() {
        let s = signals();
        assert_eq!(s.session_type(), SessionType::AudioOnly);
    }

    #[test]
    fn mark_video_sent_changes_session_type() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        assert_eq!(s.session_type(), SessionType::AudioOnly);
        s.mark_video_sent();
        assert_eq!(s.session_type(), SessionType::AudioVideo);
        assert_eq!(
            s.state.session().get::<String>("session_type"),
            Some("audio_video".into())
        );
    }

    #[test]
    fn mark_video_sent_idempotent() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.mark_video_sent();
        s.mark_video_sent();
        assert_eq!(s.session_type(), SessionType::AudioVideo);
    }

    #[test]
    fn flush_timing_after_connected() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.flush_timing();
        let elapsed: u64 = s.state.session().get("elapsed_ms").unwrap_or(0);
        assert!(elapsed < 100, "elapsed should be near zero, got {elapsed}");
        let remaining: u64 = s.state.session().get("remaining_budget_ms").unwrap();
        let limit = 15 * 60 * 1000u64;
        assert!(
            remaining > limit - 1000,
            "remaining should be near limit, got {remaining}"
        );
    }

    #[test]
    fn flush_timing_respects_video_budget() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.flush_timing();
        let remaining_audio: u64 = s.state.session().get("remaining_budget_ms").unwrap();
        assert!(remaining_audio > 14 * 60 * 1000);
        s.mark_video_sent();
        s.flush_timing();
        let remaining_video: u64 = s.state.session().get("remaining_budget_ms").unwrap();
        assert!(
            remaining_video <= 2 * 60 * 1000,
            "video remaining should be <= 120_000, got {remaining_video}"
        );
    }

    #[test]
    fn latest_resume_handle_initially_none() {
        let s = signals();
        assert_eq!(s.latest_resume_handle(), None);
    }

    #[test]
    fn latest_resume_handle_updates() {
        let s = signals();
        s.on_event(&SessionEvent::SessionResumeUpdate(
            rs_genai::session::ResumeInfo {
                handle: "h1".into(),
                resumable: true,
                last_consumed_index: None,
            },
        ));
        assert_eq!(s.latest_resume_handle(), Some("h1".to_string()));
        s.on_event(&SessionEvent::SessionResumeUpdate(
            rs_genai::session::ResumeInfo {
                handle: "h2".into(),
                resumable: true,
                last_consumed_index: Some("5".into()),
            },
        ));
        assert_eq!(s.latest_resume_handle(), Some("h2".to_string()));
    }

    #[test]
    fn silence_ms_tracked() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.flush_timing();
        let silence: u64 = s.state.session().get("silence_ms").unwrap_or(u64::MAX);
        assert!(silence < 100, "silence should be near zero, got {silence}");
    }

    #[test]
    fn audio_data_updates_activity() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::AudioData(Bytes::from_static(b"pcm")));
        s.flush_timing();
        let silence: u64 = s.state.session().get("silence_ms").unwrap_or(u64::MAX);
        assert!(silence < 100);
    }

    #[test]
    fn turn_complete_updates_activity() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::TurnComplete);
        s.flush_timing();
        let silence: u64 = s.state.session().get("silence_ms").unwrap_or(u64::MAX);
        assert!(silence < 100);
    }

    #[test]
    fn text_complete_updates_activity() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::TextComplete("done".into()));
        s.flush_timing();
        let silence: u64 = s.state.session().get("silence_ms").unwrap_or(u64::MAX);
        assert!(silence < 100);
    }

    #[test]
    fn disconnected_clears_connected_and_sets_flag() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        assert!(s.is_connected.load(Ordering::Relaxed));
        s.on_event(&SessionEvent::Disconnected(Some("server closed".into())));
        assert!(!s.is_connected.load(Ordering::Relaxed));
        assert_eq!(s.state.session().get::<bool>("disconnected"), Some(true));
    }

    #[test]
    fn disconnected_without_reason() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::Disconnected(None));
        assert!(!s.is_connected.load(Ordering::Relaxed));
        assert_eq!(s.state.session().get::<bool>("disconnected"), Some(true));
    }
}
