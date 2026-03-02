//! Auto-tracked session-level state signals.
//!
//! [`SessionSignals`] is called by the event processor on every
//! [`SessionEvent`] and transparently updates keys under the `session:`
//! prefix in the shared [`State`].  All mutation is through interior
//! mutability so `on_event` takes `&self`.

use std::sync::atomic::{AtomicBool, Ordering};
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
pub struct SessionSignals {
    state: State,
    connected_at: Mutex<Option<Instant>>,
    last_activity: Mutex<Instant>,
    has_video: AtomicBool,
    go_away_at: Mutex<Option<Instant>>,
    /// Latest resumption handle from server (persisted for reconnection).
    latest_resume_handle: Mutex<Option<String>>,
}

impl SessionSignals {
    /// Create a new `SessionSignals` backed by the given [`State`].
    pub fn new(state: State) -> Self {
        Self {
            state,
            connected_at: Mutex::new(None),
            last_activity: Mutex::new(Instant::now()),
            has_video: AtomicBool::new(false),
            go_away_at: Mutex::new(None),
            latest_resume_handle: Mutex::new(None),
        }
    }

    /// Called from the event router task.  Updates `session:*` state keys.
    ///
    /// # Single-writer invariant
    ///
    /// Must be called from a single task — counter increments (e.g.
    /// `interrupt_count`, `error_count`) use read-modify-write through
    /// [`State`] which is not atomic.
    pub fn on_event(&self, event: &SessionEvent) {
        match event {
            SessionEvent::Connected => {
                let now = Instant::now();
                *self.connected_at.lock() = Some(now);
                *self.last_activity.lock() = now;
                self.state.session().set("connected_at_ms", 0u64);
                self.state.session().set("interrupt_count", 0u64);
                self.state.session().set("error_count", 0u64);
                self.state.session().set("is_user_speaking", false);
                self.state.session().set("is_model_speaking", false);
                self.state.session().set("go_away_received", false);
                self.state.session().set("resumable", false);
                self.state
                    .session()
                    .set("session_type", "audio_only");
            }

            SessionEvent::VoiceActivityStart => {
                self.state.session().set("is_user_speaking", true);
                *self.last_activity.lock() = Instant::now();
            }

            SessionEvent::VoiceActivityEnd => {
                self.state.session().set("is_user_speaking", false);
                *self.last_activity.lock() = Instant::now();
            }

            SessionEvent::Interrupted => {
                let count: u64 = self
                    .state
                    .session()
                    .get("interrupt_count")
                    .unwrap_or(0);
                self.state.session().set("interrupt_count", count + 1);
                *self.last_activity.lock() = Instant::now();
            }

            SessionEvent::Error(msg) => {
                let count: u64 =
                    self.state.session().get("error_count").unwrap_or(0);
                self.state.session().set("error_count", count + 1);
                self.state.session().set("last_error", msg.clone());
            }

            SessionEvent::PhaseChanged(phase) => {
                self.state.session().set(
                    "is_model_speaking",
                    *phase == SessionPhase::ModelSpeaking,
                );
                self.state.session().set("phase", phase.to_string());
                *self.last_activity.lock() = Instant::now();
            }

            SessionEvent::GoAway(time_left) => {
                self.state.session().set("go_away_received", true);
                if let Some(ref tl) = time_left {
                    self.state
                        .session()
                        .set("go_away_time_left", tl.clone());
                    // Try to parse as seconds (e.g. "60s" or "60") to compute deadline.
                    if let Some(secs) = tl
                        .trim_end_matches('s')
                        .parse::<u64>()
                        .ok()
                    {
                        let deadline =
                            Instant::now() + std::time::Duration::from_secs(secs);
                        *self.go_away_at.lock() = Some(deadline);
                        self.state
                            .session()
                            .set("go_away_time_left_ms", secs * 1000);
                    }
                }
            }

            SessionEvent::SessionResumeHandle(handle) => {
                *self.latest_resume_handle.lock() = Some(handle.clone());
                self.state.session().set("resumable", true);
            }

            SessionEvent::InputTranscription(text) => {
                self.state
                    .session()
                    .set("last_input_transcription", text.clone());
                *self.last_activity.lock() = Instant::now();
            }

            SessionEvent::OutputTranscription(text) => {
                self.state
                    .session()
                    .set("last_output_transcription", text.clone());
                *self.last_activity.lock() = Instant::now();
            }

            SessionEvent::AudioData(_)
            | SessionEvent::TextDelta(_)
            | SessionEvent::TextComplete(_) => {
                // High-frequency / completion events: just update activity timer.
                *self.last_activity.lock() = Instant::now();
            }

            SessionEvent::TurnComplete => {
                *self.last_activity.lock() = Instant::now();
            }

            SessionEvent::Disconnected(_reason) => {
                // Clear connected_at so elapsed/remaining timing stops advancing.
                *self.connected_at.lock() = None;
                self.state.session().set("disconnected", true);
            }

            // Remaining variants — no special tracking needed.
            _ => {}
        }

        // ── Derived timing signals ────────────────────────────────────────

        // Silence timer
        let silence = self.last_activity.lock().elapsed().as_millis() as u64;
        self.state.session().set("silence_ms", silence);

        // Elapsed + remaining budget
        if let Some(at) = *self.connected_at.lock() {
            let elapsed_ms = at.elapsed().as_millis() as u64;
            self.state.session().set("elapsed_ms", elapsed_ms);

            let limit_ms: u64 = match self.session_type() {
                SessionType::AudioOnly => 15 * 60 * 1000,  // 15 min
                SessionType::AudioVideo => 2 * 60 * 1000,  //  2 min
            };
            let remaining = limit_ms.saturating_sub(elapsed_ms);
            self.state
                .session()
                .set("remaining_budget_ms", remaining);
        }
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use rs_genai::prelude::SessionEvent;

    fn signals() -> SessionSignals {
        SessionSignals::new(State::new())
    }

    // 1. Connected — sets connected_at, initializes state
    #[test]
    fn connected_initializes_state() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);

        assert_eq!(s.state.session().get::<u64>("connected_at_ms"), Some(0));
        assert_eq!(s.state.session().get::<u64>("interrupt_count"), Some(0));
        assert_eq!(s.state.session().get::<u64>("error_count"), Some(0));
        assert_eq!(s.state.session().get::<bool>("is_user_speaking"), Some(false));
        assert_eq!(s.state.session().get::<bool>("is_model_speaking"), Some(false));
        assert_eq!(s.state.session().get::<bool>("go_away_received"), Some(false));
        assert_eq!(s.state.session().get::<bool>("resumable"), Some(false));
        assert_eq!(
            s.state.session().get::<String>("session_type"),
            Some("audio_only".to_string())
        );
        assert!(s.connected_at.lock().is_some());
    }

    // 2. VoiceActivityStart/End — toggles is_user_speaking
    #[test]
    fn voice_activity_toggles_user_speaking() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);

        s.on_event(&SessionEvent::VoiceActivityStart);
        assert_eq!(s.state.session().get::<bool>("is_user_speaking"), Some(true));

        s.on_event(&SessionEvent::VoiceActivityEnd);
        assert_eq!(s.state.session().get::<bool>("is_user_speaking"), Some(false));
    }

    // 3. Interrupted — increments interrupt_count
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

    // 4. Error — increments error_count
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

    // 5. PhaseChanged — sets is_model_speaking
    #[test]
    fn phase_changed_sets_model_speaking() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);

        s.on_event(&SessionEvent::PhaseChanged(SessionPhase::ModelSpeaking));
        assert_eq!(s.state.session().get::<bool>("is_model_speaking"), Some(true));
        assert_eq!(
            s.state.session().get::<String>("phase"),
            Some("ModelSpeaking".into())
        );

        s.on_event(&SessionEvent::PhaseChanged(SessionPhase::Active));
        assert_eq!(s.state.session().get::<bool>("is_model_speaking"), Some(false));
        assert_eq!(
            s.state.session().get::<String>("phase"),
            Some("Active".into())
        );
    }

    // 6. GoAway — sets go_away_received + time_left
    #[test]
    fn go_away_sets_state() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);

        s.on_event(&SessionEvent::GoAway(Some("60s".into())));
        assert_eq!(s.state.session().get::<bool>("go_away_received"), Some(true));
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
        assert_eq!(s.state.session().get::<bool>("go_away_received"), Some(true));
        // No time_left keys set
        assert_eq!(s.state.session().get::<String>("go_away_time_left"), None);
        assert!(s.go_away_at.lock().is_none());
    }

    // 7. SessionResumeHandle — stores handle, sets resumable
    #[test]
    fn session_resume_handle_stored() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);

        s.on_event(&SessionEvent::SessionResumeHandle("handle-abc".into()));
        assert_eq!(s.state.session().get::<bool>("resumable"), Some(true));
        assert_eq!(s.latest_resume_handle(), Some("handle-abc".to_string()));
    }

    // 8. InputTranscription / OutputTranscription
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

        // Overwrite
        s.on_event(&SessionEvent::InputTranscription("bye".into()));
        assert_eq!(
            s.state.session().get::<String>("last_input_transcription"),
            Some("bye".into())
        );
    }

    // 9. session_type detection — mark_video_sent
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
        assert_eq!(
            s.state.session().get::<String>("session_type"),
            Some("audio_only".into())
        );

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
        s.mark_video_sent(); // second call should be a no-op
        assert_eq!(s.session_type(), SessionType::AudioVideo);
    }

    // 10. elapsed_ms and remaining_budget_ms tracking
    #[test]
    fn elapsed_and_remaining_budget_after_connected() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);

        // After Connected, elapsed should be very small
        let elapsed: u64 = s.state.session().get("elapsed_ms").unwrap_or(0);
        assert!(elapsed < 100, "elapsed should be near zero, got {elapsed}");

        let remaining: u64 = s.state.session().get("remaining_budget_ms").unwrap();
        let limit = 15 * 60 * 1000u64; // audio-only limit
        assert!(
            remaining > limit - 1000,
            "remaining should be near limit, got {remaining}"
        );
    }

    #[test]
    fn remaining_budget_changes_with_video() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);

        // Audio-only budget
        let remaining_audio: u64 =
            s.state.session().get("remaining_budget_ms").unwrap();
        assert!(remaining_audio > 14 * 60 * 1000);

        // Switch to video
        s.mark_video_sent();
        // Trigger a re-calculation by sending any event
        s.on_event(&SessionEvent::VoiceActivityStart);

        let remaining_video: u64 =
            s.state.session().get("remaining_budget_ms").unwrap();
        // Video budget is ~2 min = 120_000ms
        assert!(
            remaining_video <= 2 * 60 * 1000,
            "video remaining should be <= 120_000, got {remaining_video}"
        );
    }

    // 11. latest_resume_handle accessor
    #[test]
    fn latest_resume_handle_initially_none() {
        let s = signals();
        assert_eq!(s.latest_resume_handle(), None);
    }

    #[test]
    fn latest_resume_handle_updates() {
        let s = signals();
        s.on_event(&SessionEvent::SessionResumeHandle("h1".into()));
        assert_eq!(s.latest_resume_handle(), Some("h1".to_string()));

        s.on_event(&SessionEvent::SessionResumeHandle("h2".into()));
        assert_eq!(s.latest_resume_handle(), Some("h2".to_string()));
    }

    // Extra: silence_ms is tracked
    #[test]
    fn silence_ms_tracked() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);

        let silence: u64 = s.state.session().get("silence_ms").unwrap_or(u64::MAX);
        // Should be very small right after event
        assert!(silence < 100, "silence should be near zero, got {silence}");
    }

    // Extra: audio data updates activity
    #[test]
    fn audio_data_updates_activity() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::AudioData(Bytes::from_static(b"pcm")));

        let silence: u64 = s.state.session().get("silence_ms").unwrap_or(u64::MAX);
        assert!(silence < 100);
    }

    // Extra: turn complete updates activity
    #[test]
    fn turn_complete_updates_activity() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::TurnComplete);

        let silence: u64 = s.state.session().get("silence_ms").unwrap_or(u64::MAX);
        assert!(silence < 100);
    }

    // Extra: text complete updates activity
    #[test]
    fn text_complete_updates_activity() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        s.on_event(&SessionEvent::TextComplete("done".into()));

        let silence: u64 = s.state.session().get("silence_ms").unwrap_or(u64::MAX);
        assert!(silence < 100);
    }

    // Disconnected — clears connected_at, sets disconnected flag, stops timing
    #[test]
    fn disconnected_clears_connected_at_and_sets_flag() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);
        assert!(s.connected_at.lock().is_some());

        s.on_event(&SessionEvent::Disconnected(Some("server closed".into())));

        // connected_at should be cleared
        assert!(s.connected_at.lock().is_none());
        // disconnected flag should be set
        assert_eq!(
            s.state.session().get::<bool>("disconnected"),
            Some(true)
        );
        // elapsed_ms / remaining_budget_ms should NOT be updated after disconnect
        // (the if-let on connected_at guards this)
    }

    #[test]
    fn disconnected_without_reason() {
        let s = signals();
        s.on_event(&SessionEvent::Connected);

        s.on_event(&SessionEvent::Disconnected(None));

        assert!(s.connected_at.lock().is_none());
        assert_eq!(
            s.state.session().get::<bool>("disconnected"),
            Some(true)
        );
    }
}
