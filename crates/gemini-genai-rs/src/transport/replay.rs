//! Replay transport — feed a recorded wire log back through the session loop.
//!
//! [`ReplayTransport`] implements [`Transport`] over a recorded wire log (see
//! [`crate::transport::recording`]): `recv()` yields the recorded **inbound**
//! frames in order (as fast as the session loop consumes them), and `send()`
//! collects outbound frames for later comparison instead of touching a network.
//!
//! Because the session loop broadcasts events as soon as frames arrive, a
//! replay that starts streaming before the application has subscribed would
//! lose events nondeterministically. The transport is therefore *gated*: the
//! first `ungated_prefix` frames (default 1 — the `setupComplete` handshake)
//! are delivered immediately so the connection can reach `Active`, and the
//! rest are held until [`ReplayControl::release`] is called. Once the inbound
//! queue is exhausted the [`ReplayControl::drained`] signal fires and `recv()`
//! pends (like [`MockTransport`](super::ws::MockTransport)), keeping the
//! session alive until it is disconnected.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::watch;

use super::recording::{WireDirection, WireEntry};
use super::ws::Transport;

/// Shared collection of frames "sent" during a replay.
pub type OutboundFrames = Arc<parking_lot::Mutex<Vec<Vec<u8>>>>;

/// Errors from the [`ReplayTransport`].
#[derive(Debug, thiserror::Error)]
pub enum ReplayTransportError {
    /// Operation attempted while not connected.
    #[error("Not connected")]
    NotConnected,
}

/// Control handle for a [`ReplayTransport`] that has been moved into a
/// session loop.
#[derive(Clone)]
pub struct ReplayControl {
    gate_tx: Arc<watch::Sender<bool>>,
    drained_rx: watch::Receiver<bool>,
    outbound: OutboundFrames,
}

impl ReplayControl {
    /// Release the gated frames: inbound replay starts flowing.
    ///
    /// Call this after subscribing to the session's events so none are lost.
    pub fn release(&self) {
        let _ = self.gate_tx.send(true);
    }

    /// Wait until every recorded inbound frame has been handed to the session
    /// loop. Note: the *last* frame may still be in flight through the
    /// processor when this returns — wait for its observable effects (events,
    /// state) before asserting.
    pub async fn drained(&self) {
        let mut rx = self.drained_rx.clone();
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }

    /// Snapshot the outbound frames collected so far (in send order).
    pub fn outbound_frames(&self) -> Vec<Vec<u8>> {
        self.outbound.lock().clone()
    }
}

/// A [`Transport`] that replays recorded inbound frames and collects outbound
/// frames. See the [module docs](self) for gating and drain semantics.
pub struct ReplayTransport {
    inbound: VecDeque<Vec<u8>>,
    ungated_prefix: usize,
    delivered: usize,
    gate_rx: watch::Receiver<bool>,
    drained_tx: watch::Sender<bool>,
    outbound: OutboundFrames,
    connected: bool,
}

impl ReplayTransport {
    /// Build a replay transport from raw inbound frames.
    ///
    /// The first frame should be the `setupComplete` handshake; it is
    /// delivered ungated so the connection can reach `Active`.
    pub fn from_frames(frames: Vec<Vec<u8>>) -> (Self, ReplayControl) {
        let (gate_tx, gate_rx) = watch::channel(false);
        let (drained_tx, drained_rx) = watch::channel(false);
        let outbound: OutboundFrames = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let control = ReplayControl {
            gate_tx: Arc::new(gate_tx),
            drained_rx,
            outbound: outbound.clone(),
        };
        (
            Self {
                inbound: frames.into(),
                ungated_prefix: 1,
                delivered: 0,
                gate_rx,
                drained_tx,
                outbound,
                connected: false,
            },
            control,
        )
    }

    /// Build a replay transport from a recorded wire log, keeping only the
    /// [`WireDirection::Inbound`] entries (in log order).
    pub fn from_wire_log(entries: &[WireEntry]) -> (Self, ReplayControl) {
        let frames = entries
            .iter()
            .filter(|e| e.dir == WireDirection::Inbound)
            .map(|e| e.payload.clone())
            .collect();
        Self::from_frames(frames)
    }

    /// Override how many leading frames are delivered before
    /// [`ReplayControl::release`] (default 1: the setup handshake).
    pub fn with_ungated_prefix(mut self, n: usize) -> Self {
        self.ungated_prefix = n;
        self
    }
}

#[async_trait]
impl Transport for ReplayTransport {
    type Error = ReplayTransportError;

    async fn connect(
        &mut self,
        _url: &str,
        _headers: Vec<(String, String)>,
    ) -> Result<(), Self::Error> {
        self.connected = true;
        Ok(())
    }

    async fn send(&mut self, data: Vec<u8>) -> Result<(), Self::Error> {
        if !self.connected {
            return Err(ReplayTransportError::NotConnected);
        }
        self.outbound.lock().push(data);
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>, Self::Error> {
        if !self.connected {
            return Err(ReplayTransportError::NotConnected);
        }
        // Yield so observers can see intermediate states between frames
        // (mirrors MockTransport).
        tokio::task::yield_now().await;

        if self.inbound.is_empty() {
            let _ = self.drained_tx.send(true);
            // Stay connected-but-idle; the session loop's `select!` drops this
            // future when a command (e.g. Disconnect) arrives.
            std::future::pending::<()>().await;
            unreachable!("pending() never resolves");
        }

        if self.delivered >= self.ungated_prefix {
            let mut gate = self.gate_rx.clone();
            while !*gate.borrow() {
                // If the control handle is dropped, proceed ungated rather
                // than deadlocking the replay.
                if gate.changed().await.is_err() {
                    break;
                }
            }
        }

        let frame = self
            .inbound
            .pop_front()
            .expect("checked non-empty inbound queue");
        self.delivered += 1;
        if self.inbound.is_empty() {
            let _ = self.drained_tx.send(true);
        }
        Ok(Some(frame))
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.connected = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn replay_delivers_first_frame_ungated_then_waits_for_release() {
        let (mut transport, control) = ReplayTransport::from_frames(vec![
            br#"{"setupComplete":{}}"#.to_vec(),
            br#"{"serverContent":{"turnComplete":true}}"#.to_vec(),
        ]);
        transport.connect("replay://", vec![]).await.unwrap();

        // First frame (handshake) flows without release.
        let first = transport.recv().await.unwrap().unwrap();
        assert!(String::from_utf8(first).unwrap().contains("setupComplete"));

        // Second frame is gated.
        let gated = tokio::time::timeout(Duration::from_millis(50), transport.recv()).await;
        assert!(gated.is_err(), "second frame should be gated");

        control.release();
        let second = transport.recv().await.unwrap().unwrap();
        assert!(String::from_utf8(second).unwrap().contains("turnComplete"));

        // Drained fires once the queue is exhausted.
        tokio::time::timeout(Duration::from_millis(100), control.drained())
            .await
            .expect("drained should be signalled");

        // And recv() pends from then on.
        let idle = tokio::time::timeout(Duration::from_millis(50), transport.recv()).await;
        assert!(idle.is_err(), "recv should pend after drain");
    }

    #[tokio::test]
    async fn replay_collects_outbound_frames() {
        let (mut transport, control) =
            ReplayTransport::from_frames(vec![br#"{"setupComplete":{}}"#.to_vec()]);
        transport.connect("replay://", vec![]).await.unwrap();
        transport.send(b"{\"setup\":{}}".to_vec()).await.unwrap();
        transport
            .send(b"{\"toolResponse\":{}}".to_vec())
            .await
            .unwrap();

        let sent = control.outbound_frames();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], b"{\"setup\":{}}".to_vec());
    }

    #[tokio::test]
    async fn replay_from_wire_log_keeps_inbound_only() {
        let entries = vec![
            WireEntry {
                seq: 1,
                dir: WireDirection::Outbound,
                ts_ms: 1,
                payload: b"{\"setup\":{}}".to_vec(),
            },
            WireEntry {
                seq: 2,
                dir: WireDirection::Inbound,
                ts_ms: 2,
                payload: br#"{"setupComplete":{}}"#.to_vec(),
            },
        ];
        let (mut transport, _control) = ReplayTransport::from_wire_log(&entries);
        transport.connect("replay://", vec![]).await.unwrap();
        let first = transport.recv().await.unwrap().unwrap();
        assert!(String::from_utf8(first).unwrap().contains("setupComplete"));
    }

    #[tokio::test]
    async fn replay_errors_when_not_connected() {
        let (mut transport, _control) = ReplayTransport::from_frames(vec![]);
        assert!(transport.recv().await.is_err());
        assert!(transport.send(vec![1]).await.is_err());
    }
}
