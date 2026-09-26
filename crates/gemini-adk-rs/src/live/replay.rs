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
use gemini_genai_rs::session::{SessionEvent, SessionHandle};
use gemini_genai_rs::transport::replay::{ReplayControl, ReplayTransport};
use gemini_genai_rs::transport::{ConnectBuilder, TransportConfig, WireDirection, WireEntry};
use tokio::sync::broadcast;

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
/// Frames are delivered as fast as the session consumes them, but the
/// session reads time from a [`ManualClock`] that moves to each frame's
/// recorded capture time as it is delivered (this replaces any clock the
/// builder set). Temporal patterns, phase durations, resolver cache expiry
/// and the `session:` timing signals therefore see the original gaps between
/// frames, whatever the replay's own speed.
///
/// The session processes frames asynchronously, so before the clock jumps
/// by [`REPLAY_SYNC_STEP`] or more, the next frame waits until both event
/// lanes have handled every event of the frames before it (the router runs
/// in lockstep during a replay). An earlier frame is then evaluated at its
/// own time, not a later one. Jumps smaller than the step are not waited
/// for, which keeps dense audio fast and bounds the timing error at the
/// step. The replay is gated: nothing past
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
    // Once the session exists, the gate counts the events it has emitted
    // and waits for the lanes to have handled them all (lockstep).
    let lockstep = Arc::new(super::processor::Lockstep::default());
    let emitted: Arc<tokio::sync::Mutex<Option<EmittedCount>>> = Arc::default();
    let synced_ms = Arc::new(std::sync::atomic::AtomicU64::new(first_ts_ms));
    let gate = {
        let clock = clock.clone();
        let emitted = emitted.clone();
        let lockstep = lockstep.clone();
        Arc::new(move |ts_ms: u64| {
            let clock = clock.clone();
            let emitted = emitted.clone();
            let lockstep = lockstep.clone();
            let synced_ms = synced_ms.clone();
            Box::pin(async move {
                let step = REPLAY_SYNC_STEP.as_millis() as u64;
                let synced = synced_ms.load(std::sync::atomic::Ordering::Acquire);
                if ts_ms.saturating_sub(synced) >= step
                    && let Some((events, count)) = emitted.lock().await.as_mut()
                {
                    // The session loop takes the next frame only after it has
                    // emitted every event of the previous one, so these are
                    // all the events so far.
                    loop {
                        match events.try_recv() {
                            Ok(_) => *count += 1,
                            Err(broadcast::error::TryRecvError::Lagged(n)) => *count += n,
                            Err(_) => break,
                        }
                    }
                    lockstep.wait_for(*count, Duration::from_secs(5)).await;
                    synced_ms.store(ts_ms, std::sync::atomic::Ordering::Release);
                }
                clock.set_elapsed(Duration::from_millis(ts_ms.saturating_sub(first_ts_ms)));
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        })
    };
    let (transport, control) = ReplayTransport::from_wire_log(entries);
    let transport = transport.with_frame_gate(gate);
    let builder = builder.clock(clock.clone()).lockstep(lockstep);
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
    // Count from here, after the router subscribed: it settles everything it
    // receives, so a count started later can only trail it. Nothing past the
    // handshake flows before `release`, so no frame's events are missed.
    *emitted.lock().await = Some((handle.session().subscribe(), 0));
    Ok(ReplaySession {
        handle,
        control,
        clock,
    })
}

/// A subscription to the session's events and how many it has seen.
type EmittedCount = (broadcast::Receiver<SessionEvent>, u64);

/// The largest clock jump a replay makes without first waiting for the
/// session to process the frames already delivered. See [`replay_session`].
pub const REPLAY_SYNC_STEP: Duration = Duration::from_millis(50);

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

    #[tokio::test]
    async fn each_frame_is_processed_at_its_own_recorded_time() {
        const T0: u64 = 1_000_000_000_000;
        let turn =
            br#"{"serverContent":{"modelTurn":{"parts":[{"text":"Hi"}]},"turnComplete":true}}"#;
        let entry = |seq, ts_ms, payload: &[u8]| WireEntry {
            seq,
            dir: WireDirection::Inbound,
            ts_ms,
            payload: payload.to_vec(),
        };
        let entries = vec![
            entry(1, T0, br#"{"setupComplete":{}}"#),
            entry(2, T0 + 1_000, turn),
            entry(3, T0 + 60_000, turn),
        ];
        // A slow turn-complete handler: the lane is still on the first turn
        // when the replay would otherwise deliver the second.
        let state = crate::state::State::new();
        let seen = state.clone();
        let callbacks = crate::live::callbacks::EventCallbacks {
            on_turn_complete: Some(Arc::new(move || {
                let seen = seen.clone();
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let n = seen.get::<u32>("turns_seen").unwrap_or(0);
                    let _ = seen.set("turns_seen", n + 1);
                })
            })),
            ..Default::default()
        };
        let config = SessionConfig::new("offline").model(ModelId::LIVE_2_5_FLASH_NATIVE_AUDIO);
        let builder = LiveSessionBuilder::new(config.clone())
            .state(state)
            .callbacks(callbacks);
        let replay = replay_session(config, builder, &entries).await.unwrap();
        let mut events = replay.handle().events();
        replay.release();
        replay.drained().await;
        collect_events_until_idle(
            &mut events,
            Duration::from_millis(400),
            Duration::from_secs(5),
        )
        .await;

        let recorded = |ms: u64| UNIX_EPOCH + Duration::from_millis(ms);
        let stamps: Vec<_> = replay
            .handle()
            .state()
            .recent_mutations()
            .iter()
            .filter(|m| m.key == "turns_seen")
            .map(|m| m.timestamp)
            .collect();
        assert_eq!(
            stamps,
            [recorded(T0 + 1_000), recorded(T0 + 60_000)],
            "each turn's handler runs at its own recorded time"
        );
        replay.disconnect().await.unwrap();
    }
}
