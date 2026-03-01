//! Call lifecycle management — inbound/outbound calls with state tracking.
//!
//! [`CallSession`] wraps a [`SessionHandle`] + [`AudioPipeline`] with a
//! call-level FSM ([`CallPhase`]) and metrics ([`CallMetrics`]).
//!
//! # Example
//!
//! ```rust,no_run
//! # use gemini_live_rs::prelude::*;
//! # use gemini_live_rs::call::*;
//! # use gemini_live_rs::pipeline::*;
//! # async fn run(agent: GeminiAgent, source: Box<dyn AudioSource>, sink: Box<dyn AudioSink>) -> Result<(), Box<dyn std::error::Error>> {
//! let call = CallSession::inbound(agent.session().clone(), agent.pipeline_config.clone(), source, sink).await?;
//! call.send_text("Welcome! How can I help you?").await?;
//! call.wait_until_done().await;
//! let metrics = call.metrics();
//! println!("Call lasted {:?}, {} turns", metrics.duration(), metrics.turn_count);
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::app::PipelineConfig;
use crate::pipeline::{AudioPipeline, AudioSink, AudioSource};
use crate::session::{SessionError, SessionEvent, SessionHandle, SessionPhase};

// ---------------------------------------------------------------------------
// Call phase FSM
// ---------------------------------------------------------------------------

/// High-level call lifecycle phase (layered on top of [`SessionPhase`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPhase {
    /// Call not yet started.
    Idle,
    /// Inbound call ringing, waiting for accept.
    Ringing,
    /// Outbound call dialing, waiting for answer.
    Dialing,
    /// Call is active — audio flowing, conversation in progress.
    Active,
    /// Call is on hold (audio muted, pipeline paused).
    OnHold,
    /// Call is being transferred to another agent/session.
    Transferring,
    /// Call has ended.
    Ended,
}

impl CallPhase {
    /// Check if a transition to the given phase is valid.
    pub fn can_transition_to(&self, to: &CallPhase) -> bool {
        matches!(
            (self, to),
            // Start
            (CallPhase::Idle, CallPhase::Ringing)
                | (CallPhase::Idle, CallPhase::Dialing)
                // Connect
                | (CallPhase::Ringing, CallPhase::Active)
                | (CallPhase::Ringing, CallPhase::Ended)
                | (CallPhase::Dialing, CallPhase::Active)
                | (CallPhase::Dialing, CallPhase::Ended)
                // Active lifecycle
                | (CallPhase::Active, CallPhase::Ended)
                | (CallPhase::Active, CallPhase::OnHold)
                | (CallPhase::Active, CallPhase::Transferring)
                // Hold
                | (CallPhase::OnHold, CallPhase::Active)
                | (CallPhase::OnHold, CallPhase::Ended)
                // Transfer
                | (CallPhase::Transferring, CallPhase::Ended)
                | (CallPhase::Transferring, CallPhase::Active) // transfer failed
        )
    }
}

impl std::fmt::Display for CallPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallPhase::Idle => write!(f, "Idle"),
            CallPhase::Ringing => write!(f, "Ringing"),
            CallPhase::Dialing => write!(f, "Dialing"),
            CallPhase::Active => write!(f, "Active"),
            CallPhase::OnHold => write!(f, "OnHold"),
            CallPhase::Transferring => write!(f, "Transferring"),
            CallPhase::Ended => write!(f, "Ended"),
        }
    }
}

// ---------------------------------------------------------------------------
// Call metrics
// ---------------------------------------------------------------------------

/// Metrics tracked for a single call.
#[derive(Debug, Clone)]
pub struct CallMetrics {
    /// When the call entered Active.
    pub started_at: Option<Instant>,
    /// When the call ended.
    pub ended_at: Option<Instant>,
    /// Total model turns completed.
    pub turn_count: u32,
    /// Number of barge-in interruptions.
    pub interruption_count: u32,
    /// Number of tool calls dispatched.
    pub tool_call_count: u32,
    /// Names of tools used (for analytics).
    pub tools_used: Vec<String>,
    /// Total time spent on hold.
    pub hold_duration: Duration,
}

impl CallMetrics {
    /// Create empty metrics.
    pub fn new() -> Self {
        Self {
            started_at: None,
            ended_at: None,
            turn_count: 0,
            interruption_count: 0,
            tool_call_count: 0,
            tools_used: Vec::new(),
            hold_duration: Duration::ZERO,
        }
    }

    /// Total call duration (Active to Ended).
    pub fn duration(&self) -> Duration {
        match (self.started_at, self.ended_at) {
            (Some(s), Some(e)) => e.duration_since(s),
            (Some(s), None) => s.elapsed(),
            _ => Duration::ZERO,
        }
    }
}

impl Default for CallMetrics {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Call session
// ---------------------------------------------------------------------------

/// A managed voice call session with lifecycle tracking.
///
/// Integrates:
/// - [`AudioPipeline`] (VAD + barge-in + turn detection + jitter buffer)
/// - [`CallPhase`] FSM (Idle → Ringing/Dialing → Active ↔ OnHold → Ended)
/// - [`CallMetrics`] (duration, turns, interruptions, tool usage)
pub struct CallSession {
    handle: SessionHandle,
    pipeline: Option<AudioPipeline>,
    phase: Arc<Mutex<CallPhase>>,
    metrics: Arc<Mutex<CallMetrics>>,
    hold_started: Arc<Mutex<Option<Instant>>>,
    event_handle: Option<tokio::task::JoinHandle<()>>,
}

impl CallSession {
    /// Create a call session for an inbound call.
    ///
    /// Waits for the underlying Gemini session to be active, then starts
    /// the audio pipeline immediately.
    pub async fn inbound(
        handle: SessionHandle,
        config: PipelineConfig,
        source: Box<dyn AudioSource>,
        sink: Box<dyn AudioSink>,
    ) -> Result<Self, SessionError> {
        let phase = Arc::new(Mutex::new(CallPhase::Ringing));
        let metrics = Arc::new(Mutex::new(CallMetrics::new()));
        let hold_started = Arc::new(Mutex::new(None));

        // Wait for transport to be ready
        handle.wait_for_phase(SessionPhase::Active).await;

        // Transition to Active
        *phase.lock() = CallPhase::Active;
        metrics.lock().started_at = Some(Instant::now());

        // Start audio pipeline
        let pipeline = AudioPipeline::start(handle.clone(), config, source, sink);

        // Spawn metrics tracker
        let event_handle = Self::spawn_metrics_tracker(
            handle.clone(),
            metrics.clone(),
            phase.clone(),
        );

        Ok(Self {
            handle,
            pipeline: Some(pipeline),
            phase,
            metrics,
            hold_started,
            event_handle: Some(event_handle),
        })
    }

    /// Create a call session for an outbound call.
    ///
    /// Functionally identical to [`inbound`](CallSession::inbound) but sets
    /// the initial phase to [`CallPhase::Dialing`].
    pub async fn outbound(
        handle: SessionHandle,
        config: PipelineConfig,
        source: Box<dyn AudioSource>,
        sink: Box<dyn AudioSink>,
    ) -> Result<Self, SessionError> {
        let phase = Arc::new(Mutex::new(CallPhase::Dialing));
        let metrics = Arc::new(Mutex::new(CallMetrics::new()));
        let hold_started = Arc::new(Mutex::new(None));

        handle.wait_for_phase(SessionPhase::Active).await;

        *phase.lock() = CallPhase::Active;
        metrics.lock().started_at = Some(Instant::now());

        let pipeline = AudioPipeline::start(handle.clone(), config, source, sink);
        let event_handle = Self::spawn_metrics_tracker(
            handle.clone(),
            metrics.clone(),
            phase.clone(),
        );

        Ok(Self {
            handle,
            pipeline: Some(pipeline),
            phase,
            metrics,
            hold_started,
            event_handle: Some(event_handle),
        })
    }

    /// Background task that tracks call metrics from session events.
    fn spawn_metrics_tracker(
        handle: SessionHandle,
        metrics: Arc<Mutex<CallMetrics>>,
        phase: Arc<Mutex<CallPhase>>,
    ) -> tokio::task::JoinHandle<()> {
        let mut events = handle.subscribe();
        tokio::spawn(async move {
            while let Ok(event) = events.recv().await {
                match event {
                    SessionEvent::TurnComplete => {
                        metrics.lock().turn_count += 1;
                    }
                    SessionEvent::Interrupted => {
                        metrics.lock().interruption_count += 1;
                    }
                    SessionEvent::ToolCall(calls) => {
                        let mut m = metrics.lock();
                        m.tool_call_count += calls.len() as u32;
                        for call in &calls {
                            m.tools_used.push(call.name.clone());
                        }
                    }
                    SessionEvent::Disconnected(_) => {
                        metrics.lock().ended_at = Some(Instant::now());
                        *phase.lock() = CallPhase::Ended;
                        break;
                    }
                    _ => {}
                }
            }
        })
    }

    /// Current call phase.
    pub fn phase(&self) -> CallPhase {
        *self.phase.lock()
    }

    /// Current call metrics snapshot.
    pub fn metrics(&self) -> CallMetrics {
        self.metrics.lock().clone()
    }

    /// Access the underlying session handle.
    pub fn session(&self) -> &SessionHandle {
        &self.handle
    }

    /// Send text during the call.
    pub async fn send_text(
        &self,
        text: impl Into<String>,
    ) -> Result<(), SessionError> {
        self.handle.send_text(text).await
    }

    /// Put the call on hold — stops the audio pipeline.
    pub async fn hold(&mut self) -> Result<(), SessionError> {
        {
            let mut phase = self.phase.lock();
            if !phase.can_transition_to(&CallPhase::OnHold) {
                return Err(SessionError::SetupFailed(format!(
                    "Cannot hold from phase {}",
                    *phase,
                )));
            }
            *phase = CallPhase::OnHold;
        }
        *self.hold_started.lock() = Some(Instant::now());

        // Stop the pipeline
        if let Some(pipeline) = self.pipeline.take() {
            pipeline.stop().await;
        }
        Ok(())
    }

    /// Resume from hold — restarts the audio pipeline with new source/sink.
    pub async fn resume(
        &mut self,
        config: PipelineConfig,
        source: Box<dyn AudioSource>,
        sink: Box<dyn AudioSink>,
    ) -> Result<(), SessionError> {
        {
            let mut phase = self.phase.lock();
            if !phase.can_transition_to(&CallPhase::Active) {
                return Err(SessionError::SetupFailed(format!(
                    "Cannot resume from phase {}",
                    *phase,
                )));
            }
            *phase = CallPhase::Active;
        }

        // Track hold duration
        if let Some(started) = self.hold_started.lock().take() {
            self.metrics.lock().hold_duration += started.elapsed();
        }

        // Restart pipeline
        self.pipeline = Some(AudioPipeline::start(
            self.handle.clone(),
            config,
            source,
            sink,
        ));
        Ok(())
    }

    /// Hang up the call — stops the pipeline, disconnects, returns final metrics.
    pub async fn hangup(mut self) -> Result<CallMetrics, SessionError> {
        *self.phase.lock() = CallPhase::Ended;
        self.metrics.lock().ended_at = Some(Instant::now());

        // Stop pipeline first
        if let Some(pipeline) = self.pipeline.take() {
            pipeline.stop().await;
        }

        // Disconnect session
        self.handle.disconnect().await?;

        // Wait for metrics tracker
        if let Some(h) = self.event_handle.take() {
            let _ = h.await;
        }

        Ok(self.metrics.lock().clone())
    }

    /// Wait until the call ends (remote hangup or error).
    pub async fn wait_until_done(&self) {
        self.handle
            .wait_for_phase(SessionPhase::Disconnected)
            .await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_phase_transitions() {
        assert!(CallPhase::Idle.can_transition_to(&CallPhase::Ringing));
        assert!(CallPhase::Idle.can_transition_to(&CallPhase::Dialing));
        assert!(CallPhase::Ringing.can_transition_to(&CallPhase::Active));
        assert!(CallPhase::Ringing.can_transition_to(&CallPhase::Ended));
        assert!(CallPhase::Active.can_transition_to(&CallPhase::OnHold));
        assert!(CallPhase::Active.can_transition_to(&CallPhase::Transferring));
        assert!(CallPhase::Active.can_transition_to(&CallPhase::Ended));
        assert!(CallPhase::OnHold.can_transition_to(&CallPhase::Active));
        assert!(CallPhase::OnHold.can_transition_to(&CallPhase::Ended));
        assert!(CallPhase::Transferring.can_transition_to(&CallPhase::Ended));
        assert!(CallPhase::Transferring.can_transition_to(&CallPhase::Active));
    }

    #[test]
    fn call_phase_invalid_transitions() {
        assert!(!CallPhase::Idle.can_transition_to(&CallPhase::Active));
        assert!(!CallPhase::Idle.can_transition_to(&CallPhase::Ended));
        assert!(!CallPhase::Ended.can_transition_to(&CallPhase::Active));
        assert!(!CallPhase::Ringing.can_transition_to(&CallPhase::OnHold));
        assert!(!CallPhase::Dialing.can_transition_to(&CallPhase::OnHold));
    }

    #[test]
    fn call_metrics_defaults() {
        let m = CallMetrics::new();
        assert!(m.started_at.is_none());
        assert!(m.ended_at.is_none());
        assert_eq!(m.turn_count, 0);
        assert_eq!(m.interruption_count, 0);
        assert_eq!(m.tool_call_count, 0);
        assert!(m.tools_used.is_empty());
        assert_eq!(m.hold_duration, Duration::ZERO);
        assert_eq!(m.duration(), Duration::ZERO);
    }

    #[test]
    fn call_metrics_duration() {
        let start = Instant::now();
        let end = start + Duration::from_secs(5);
        let m = CallMetrics {
            started_at: Some(start),
            ended_at: Some(end),
            ..CallMetrics::new()
        };
        assert_eq!(m.duration(), Duration::from_secs(5));
    }

    #[test]
    fn call_phase_display() {
        assert_eq!(CallPhase::Active.to_string(), "Active");
        assert_eq!(CallPhase::OnHold.to_string(), "OnHold");
        assert_eq!(CallPhase::Ended.to_string(), "Ended");
    }
}
