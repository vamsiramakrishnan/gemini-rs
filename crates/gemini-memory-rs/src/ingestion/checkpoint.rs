//! Session cadence: when to micro-reconcile, when to checkpoint, when to seal.
//!
//! A long conversation crossing several transport connections must not lose an
//! hour of evidence to one unclean disconnect, and must not pay for a full
//! reconciliation every time a WebSocket reconnects. Checkpointing bounds the
//! recovery cost without ending the logical session.

use chrono::{DateTime, Duration, Utc};

use crate::core::{CadenceConfig, MemoryRuntimeConfig, SessionConfig};

/// Work the cadence says is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledWork {
    /// Merge duplicates and resolve in-session contradictions.
    MicroReconcile,
    /// Persist pending evidence and refresh the index, session still live.
    Checkpoint,
    /// The session has gone idle; seal it and reconcile fully.
    SealSession,
}

/// Tracks turn counts and elapsed time against the configured cadences.
#[derive(Debug)]
pub struct CadenceTracker {
    micro: CadenceConfig,
    checkpoint: CadenceConfig,
    session: SessionConfig,
    turns_since_micro: u32,
    turns_since_checkpoint: u32,
    last_micro_at: DateTime<Utc>,
    last_checkpoint_at: DateTime<Utc>,
    last_activity_at: DateTime<Utc>,
    total_turns: u64,
    sealed: bool,
}

impl CadenceTracker {
    /// Start tracking from `now`.
    pub fn new(config: &MemoryRuntimeConfig, now: DateTime<Utc>) -> Self {
        Self {
            micro: config.micro_reconciliation,
            checkpoint: config.checkpoint,
            session: config.session,
            turns_since_micro: 0,
            turns_since_checkpoint: 0,
            last_micro_at: now,
            last_checkpoint_at: now,
            last_activity_at: now,
            total_turns: 0,
            sealed: false,
        }
    }

    /// Turns completed in this logical session.
    pub fn total_turns(&self) -> u64 {
        self.total_turns
    }

    /// Whether the session has been sealed.
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// Record activity without completing a turn — this defers idle sealing.
    pub fn touch(&mut self, now: DateTime<Utc>) {
        self.last_activity_at = now;
    }

    /// Record a completed user turn and report what is now due.
    ///
    /// Checkpoint implies micro-reconciliation, so only the larger unit is
    /// reported when both come due on the same turn.
    pub fn on_turn_complete(&mut self, now: DateTime<Utc>) -> Vec<ScheduledWork> {
        self.total_turns += 1;
        self.turns_since_micro += 1;
        self.turns_since_checkpoint += 1;
        self.last_activity_at = now;

        let mut due = Vec::new();
        if self
            .checkpoint
            .is_due(self.turns_since_checkpoint, now - self.last_checkpoint_at)
        {
            self.turns_since_checkpoint = 0;
            self.last_checkpoint_at = now;
            self.turns_since_micro = 0;
            self.last_micro_at = now;
            due.push(ScheduledWork::Checkpoint);
        } else if self
            .micro
            .is_due(self.turns_since_micro, now - self.last_micro_at)
        {
            self.turns_since_micro = 0;
            self.last_micro_at = now;
            due.push(ScheduledWork::MicroReconcile);
        }
        due
    }

    /// Whether the session has been idle long enough to seal.
    pub fn is_idle(&self, now: DateTime<Utc>) -> bool {
        !self.sealed
            && (now - self.last_activity_at)
                >= Duration::seconds(self.session.logical_idle_timeout_seconds as i64)
    }

    /// Report sealing work if the session has gone idle.
    ///
    /// A transport reconnect is deliberately not an input here: a WebSocket
    /// closing is not the user ending the conversation.
    pub fn poll_idle(&mut self, now: DateTime<Utc>) -> Option<ScheduledWork> {
        if self.is_idle(now) {
            self.sealed = true;
            Some(ScheduledWork::SealSession)
        } else {
            None
        }
    }

    /// Seal explicitly, e.g. because the user ended the conversation.
    pub fn seal(&mut self) {
        self.sealed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker(now: DateTime<Utc>) -> CadenceTracker {
        CadenceTracker::new(&MemoryRuntimeConfig::default(), now)
    }

    #[test]
    fn micro_reconciliation_fires_on_the_configured_turn_count() {
        let now = Utc::now();
        let mut tracker = tracker(now);
        for turn in 1..4 {
            assert!(
                tracker.on_turn_complete(now).is_empty(),
                "nothing due at turn {turn}"
            );
        }
        assert_eq!(
            tracker.on_turn_complete(now),
            vec![ScheduledWork::MicroReconcile]
        );
    }

    #[test]
    fn micro_reconciliation_also_fires_on_elapsed_time() {
        let now = Utc::now();
        let mut tracker = tracker(now);
        assert_eq!(
            tracker.on_turn_complete(now + Duration::seconds(120)),
            vec![ScheduledWork::MicroReconcile]
        );
    }

    #[test]
    fn a_checkpoint_subsumes_the_micro_pass_it_coincides_with() {
        let now = Utc::now();
        let mut tracker = tracker(now);
        let mut checkpoints = 0;
        let mut micros = 0;
        for _ in 0..20 {
            for work in tracker.on_turn_complete(now) {
                match work {
                    ScheduledWork::Checkpoint => checkpoints += 1,
                    ScheduledWork::MicroReconcile => micros += 1,
                    ScheduledWork::SealSession => unreachable!(),
                }
            }
        }
        assert_eq!(checkpoints, 1, "one checkpoint in twenty turns");
        assert_eq!(micros, 4, "turn 20 reports the checkpoint, not both");
        assert_eq!(tracker.total_turns(), 20);
    }

    #[test]
    fn an_idle_session_seals_exactly_once() {
        let now = Utc::now();
        let mut tracker = tracker(now);
        tracker.on_turn_complete(now);

        assert!(tracker.poll_idle(now + Duration::seconds(60)).is_none());
        assert_eq!(
            tracker.poll_idle(now + Duration::seconds(200)),
            Some(ScheduledWork::SealSession)
        );
        assert!(tracker.poll_idle(now + Duration::seconds(400)).is_none());
        assert!(tracker.is_sealed());
    }

    #[test]
    fn activity_defers_idle_sealing() {
        let now = Utc::now();
        let mut tracker = tracker(now);
        tracker.on_turn_complete(now);
        tracker.touch(now + Duration::seconds(170));
        assert!(tracker.poll_idle(now + Duration::seconds(200)).is_none());
        assert!(tracker.poll_idle(now + Duration::seconds(360)).is_some());
    }
}
