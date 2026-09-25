//! Offline session replay — feed a recorded wire log through the **real**
//! control plane.
//!
//! Recording happens at L0 via
//! [`SessionConfig::record_wire`](gemini_genai_rs::prelude::SessionConfig::record_wire)
//! (every wire byte, both directions, as [`WireEntry`] JSONL). This module
//! closes the loop: [`replay_session`] opens a
//! [`ReplayTransport`] over the log's inbound frames and attaches the same three-lane processor a
//! live connection would get — phase machine, extractors, watchers, tool
//! dispatch, flow governance all run for real. Nothing is mocked above the
//! transport seam.
//!
//! What replay does and does not do:
//!
//! - **Does**: re-decode every recorded inbound frame, re-drive the L1
//!   processor (events, state writes, tool dispatch through whatever
//!   dispatcher you attach), and collect the outbound frames the processor
//!   regenerates (setup, tool responses) for comparison against the log.
//! - **Does not**: re-execute the model. The model's outputs are *in* the
//!   recorded inbound frames. User-originated sends (text/audio) are in the
//!   log's outbound entries but are not re-sent — they only ever existed to
//!   provoke the recorded inbound frames.
//!
//! ```rust,no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use gemini_adk_rs::live::replay::replay_session;
//! use gemini_adk_rs::live::LiveSessionBuilder;
//! use gemini_genai_rs::prelude::SessionConfig;
//! use gemini_genai_rs::transport::read_wire_log;
//!
//! let entries = read_wire_log("session.wire.jsonl")?;
//! let config = SessionConfig::new("offline");
//! let builder = LiveSessionBuilder::new(config.clone());
//! let replay = replay_session(config, builder, &entries).await?;
//!
//! let mut events = replay.handle().events();
//! replay.release(); // start streaming recorded frames
//! replay.drained().await; // all frames handed to the session loop
//! # let _ = events.try_recv();
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use gemini_genai_rs::prelude::{SessionConfig, SessionPhase};
use gemini_genai_rs::session::SessionHandle;
use gemini_genai_rs::transport::replay::{ReplayControl, ReplayTransport};
use gemini_genai_rs::transport::{ConnectBuilder, TransportConfig, WireDirection, WireEntry};

use crate::clock::ManualClock;

use crate::error::AgentError;

use super::builder::{LiveSessionBuilder, build_runtime, spawn_lanes};
use super::events::LiveEvent;
use super::handle::LiveHandle;

/// Attach the full L1 control plane (three-lane processor, phase machine,
/// extractors, watchers, tool dispatch, …) to an **already connected** L0
/// session.
///
/// This is the seam that makes replay possible without touching the network:
/// connect the L0 session over any [`Transport`](gemini_genai_rs::transport::Transport)
/// (e.g. [`ReplayTransport`] or [`MockTransport`](gemini_genai_rs::transport::MockTransport)), then hand
/// it here together with a configured [`LiveSessionBuilder`].
///
/// Note: the builder's own `SessionConfig` is *not* re-sent — the setup
/// message was already encoded from the config given to the L0 connect call.
/// Subscribe to events **after** this returns and only then let the transport
/// stream (for `ReplayTransport`, call
/// [`ReplayControl::release`](gemini_genai_rs::transport::replay::ReplayControl::release)),
/// otherwise early frames race the subscription.
pub async fn attach_session(
    builder: LiveSessionBuilder,
    session: SessionHandle,
) -> Result<LiveHandle, AgentError> {
    let plan = builder.into_plan()?;
    session.wait_for_phase(SessionPhase::Active).await;
    let runtime = build_runtime(plan, session);
    spawn_lanes(runtime).await
}

/// A replayed session: the live handle plus the replay controls.
pub struct ReplaySession {
    handle: LiveHandle,
    control: ReplayControl,
    clock: Arc<ManualClock>,
}

impl ReplaySession {
    /// The live handle — same type a real connection returns. `state()`,
    /// `events()`, `telemetry()`, `extracted()` all work.
    pub fn handle(&self) -> &LiveHandle {
        &self.handle
    }

    /// Start streaming the recorded inbound frames. Call after subscribing
    /// to [`LiveHandle::events`].
    pub fn release(&self) {
        self.control.release();
    }

    /// Wait until every recorded inbound frame has been handed to the session
    /// loop. The last frame's effects may still be propagating through the
    /// processor — use [`collect_events_until_idle`] (or assert on state) to
    /// settle.
    pub async fn drained(&self) {
        self.control.drained().await;
    }

    /// Outbound frames the replayed session has sent so far (setup, tool
    /// responses, …), in send order, for comparison against the recorded log.
    pub fn outbound_frames(&self) -> Vec<Vec<u8>> {
        self.control.outbound_frames()
    }

    /// The replay's clock. It stands at the recorded capture time of the most
    /// recently delivered frame, measured from the first inbound frame.
    pub fn clock(&self) -> &Arc<ManualClock> {
        &self.clock
    }

    /// Disconnect the replayed session.
    pub async fn disconnect(&self) -> Result<(), gemini_genai_rs::session::SessionError> {
        self.handle.disconnect().await
    }
}

/// Replay a recorded wire log through the real L1 processor, offline.
///
/// - `config` is used to open the replay transport (its re-encoded setup
///   message becomes the first outbound frame, mirroring the original run).
///   Use the same configuration as the recorded session for a faithful setup
///   comparison. No network is touched and no credential is used.
/// - `builder` supplies the control plane: dispatcher, phases, extractors,
///   watchers, state, callbacks. Attach the original tool implementations to
///   re-execute tools deterministically; without a dispatcher, recorded tool
///   calls surface as events but produce no responses.
/// - `entries` is the recorded log; only its inbound frames are replayed
///   (outbound entries are kept in the log purely for comparison/audit).
///
/// Frames are delivered as fast as the session loop consumes them, but the
/// session reads time from a [`ManualClock`] that moves to each frame's
/// recorded capture time as it is delivered (this replaces any clock the
/// builder set). Temporal patterns, phase durations, resolver cache expiry
/// and the `session:` timing signals therefore see the original gaps between
/// frames, whatever the replay's own speed. The replay is gated: nothing past
/// the setup handshake flows until [`ReplaySession::release`] is called, so
/// subscribe to events first.
pub async fn replay_session(
    config: SessionConfig,
    builder: LiveSessionBuilder,
    entries: &[WireEntry],
) -> Result<ReplaySession, AgentError> {
    let first_ts_ms = entries
        .iter()
        .find(|e| e.dir == WireDirection::Inbound)
        .map_or(0, |e| e.ts_ms);
    let clock = Arc::new(ManualClock::starting_at(
        UNIX_EPOCH + Duration::from_millis(first_ts_ms),
    ));
    let observed = clock.clone();
    let (transport, control) = ReplayTransport::from_wire_log(entries);
    let transport = transport.with_frame_observer(Arc::new(move |ts_ms| {
        observed.set_elapsed(Duration::from_millis(ts_ms.saturating_sub(first_ts_ms)));
    }));
    let builder = builder.clock(clock.clone());
    let transport_config = TransportConfig {
        max_reconnect_attempts: 0,
        connect_timeout_secs: 5,
        setup_timeout_secs: 5,
        ..TransportConfig::default()
    };
    let session = ConnectBuilder::new(config)
        .transport_config(transport_config)
        .transport(transport)
        .connect()
        .await
        .map_err(AgentError::Session)?;
    let handle = attach_session(builder, session).await?;
    Ok(ReplaySession {
        handle,
        control,
        clock,
    })
}

/// Collect [`LiveEvent`]s until the stream stays idle for `idle` (or `max`
/// elapses). Useful for settling an as-fast-as-possible replay where "done"
/// means "no more effects are propagating".
pub async fn collect_events_until_idle(
    rx: &mut tokio::sync::broadcast::Receiver<LiveEvent>,
    idle: Duration,
    max: Duration,
) -> Vec<LiveEvent> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + max;
    loop {
        let timeout = idle.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
        if timeout.is_zero() {
            break;
        }
        match tokio::time::timeout(timeout, rx.recv()).await {
            Ok(Ok(event)) => events.push(event),
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
            Err(_) => break, // idle
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use gemini_genai_rs::prelude::ModelId;

    #[tokio::test]
    async fn replay_session_reaches_active_and_emits_events() {
        let entries = vec![
            WireEntry {
                seq: 1,
                dir: WireDirection::Inbound,
                ts_ms: 1,
                payload: br#"{"setupComplete":{}}"#.to_vec(),
            },
            WireEntry {
                seq: 2,
                dir: WireDirection::Inbound,
                ts_ms: 2,
                payload:
                    br#"{"serverContent":{"modelTurn":{"parts":[{"text":"Hi"}]},"turnComplete":true}}"#
                        .to_vec(),
            },
        ];
        let config = SessionConfig::new("offline").model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO);
        let builder = LiveSessionBuilder::new(config.clone());

        let replay = replay_session(config, builder, &entries).await.unwrap();
        let mut events = replay.handle().events();
        replay.release();
        replay.drained().await;

        let collected = collect_events_until_idle(
            &mut events,
            Duration::from_millis(200),
            Duration::from_secs(5),
        )
        .await;

        assert!(
            collected
                .iter()
                .any(|e| matches!(e, LiveEvent::TextDelta(t) if t == "Hi")),
            "expected replayed TextDelta, got {collected:?}"
        );
        assert!(
            collected
                .iter()
                .any(|e| matches!(e, LiveEvent::TurnComplete))
        );

        // The clock stands at the last frame's recorded time.
        assert_eq!(replay.clock().elapsed(), Duration::from_millis(1));

        // The replayed session re-encoded and "sent" the setup message.
        let outbound = replay.outbound_frames();
        assert!(!outbound.is_empty());
        assert!(
            String::from_utf8(outbound[0].clone())
                .unwrap()
                .contains("\"setup\"")
        );

        replay.disconnect().await.unwrap();
    }

    /// Timing decisions follow the recording, not the replay's speed: a turn
    /// recorded ten seconds after the handshake is stamped ten seconds later,
    /// at the recording's wall time, even though the replay takes
    /// milliseconds.
    #[tokio::test]
    async fn replay_reads_time_from_the_recording() {
        const T0: u64 = 1_000_000_000_000; // 2001-09-09, well before "now"
        let entries = vec![
            WireEntry {
                seq: 1,
                dir: WireDirection::Inbound,
                ts_ms: T0,
                payload: br#"{"setupComplete":{}}"#.to_vec(),
            },
            WireEntry {
                seq: 2,
                dir: WireDirection::Inbound,
                ts_ms: T0 + 10_000,
                payload:
                    br#"{"serverContent":{"modelTurn":{"parts":[{"text":"Hi"}]},"turnComplete":true}}"#
                        .to_vec(),
            },
        ];
        let config = SessionConfig::new("offline").model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO);
        let replay = replay_session(config.clone(), LiveSessionBuilder::new(config), &entries)
            .await
            .unwrap();
        let mut events = replay.handle().events();
        replay.release();
        replay.drained().await;
        collect_events_until_idle(
            &mut events,
            Duration::from_millis(200),
            Duration::from_secs(5),
        )
        .await;

        assert_eq!(replay.clock().elapsed(), Duration::from_secs(10));
        let mutations = replay.handle().state().recent_mutations();
        assert!(!mutations.is_empty(), "the turn should write state");
        let recorded = |ms: u64| UNIX_EPOCH + Duration::from_millis(ms);
        for m in &mutations {
            assert!(
                m.timestamp >= recorded(T0) && m.timestamp <= recorded(T0 + 10_000),
                "{} stamped outside the recording: {:?}",
                m.key,
                m.timestamp
            );
        }
        assert!(
            mutations
                .iter()
                .any(|m| m.timestamp == recorded(T0 + 10_000)),
            "the turn's writes carry the second frame's recorded time"
        );

        replay.disconnect().await.unwrap();
    }
}
