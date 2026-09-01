//! Pattern promotion: staged inferences that have earned durability.
//!
//! Three mentions inside one conversation is repetition — or a model
//! misunderstanding the same sentence three times. Evidence spread across
//! sessions *and* days is what distinguishes a stable pattern from an artefact
//! of one conversation, which is why the promotion bar counts both.

use chrono::{DateTime, Duration, Utc};

use crate::core::{
    CanonicalMemory, Explicitness, MemoryStatus, PromotionConfig, PromotionEvidence,
    meets_promotion_criteria,
};

/// What a promotion sweep decided about one staged record.
#[derive(Debug, Clone, PartialEq)]
pub enum PromotionOutcome {
    /// Meets the bar; write it active.
    Promote(Box<CanonicalMemory>),
    /// Not yet; leave it staged.
    Hold {
        /// Why it did not qualify.
        reason: PromotionShortfall,
    },
    /// Stale beyond the retention window; drop it.
    Expire(Box<CanonicalMemory>),
}

/// Why a staged record was not promoted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionShortfall {
    /// Not enough supporting observations.
    Evidence,
    /// Seen in too few logical sessions.
    Sessions,
    /// Seen on too few days.
    Days,
    /// Aggregated confidence below the bar.
    Confidence,
    /// Sensitive, and never stated outright.
    RequiresExplicitStatement,
}

/// How long a staged pattern may sit unreinforced before it is dropped.
///
/// Staging is not a graveyard: an inference nothing has confirmed in three
/// months was probably wrong, and keeping it costs retrieval quality.
pub const STAGING_RETENTION_DAYS: i64 = 90;

/// Evaluate one staged record.
pub fn evaluate(
    staged: &CanonicalMemory,
    config: &PromotionConfig,
    now: DateTime<Utc>,
) -> PromotionOutcome {
    let explicitness = if staged.source.is_explicit() {
        Explicitness::ExplicitStatement
    } else {
        Explicitness::StrongImplication
    };

    let evidence = PromotionEvidence {
        evidence_count: staged.evidence.count,
        distinct_sessions: staged.evidence.distinct_sessions,
        distinct_days: staged.evidence.distinct_days,
        confidence: staged.confidence,
        has_unresolved_contradiction: false,
        sensitivity: staged.privacy.sensitivity,
        strongest_explicitness: explicitness,
    };

    if meets_promotion_criteria(&evidence, config) {
        let mut promoted = staged.clone();
        promoted.status = MemoryStatus::Active;
        promoted.temporal.updated_at = now;
        promoted.evidence_summary = format!(
            "Promoted from a staged pattern after {} observations across {} sessions and {} days.",
            evidence.evidence_count, evidence.distinct_sessions, evidence.distinct_days
        );
        return PromotionOutcome::Promote(Box::new(promoted));
    }

    if now - staged.temporal.last_confirmed_at > Duration::days(STAGING_RETENTION_DAYS) {
        return PromotionOutcome::Expire(Box::new(staged.clone()));
    }

    PromotionOutcome::Hold {
        reason: shortfall(&evidence, config),
    }
}

fn shortfall(evidence: &PromotionEvidence, config: &PromotionConfig) -> PromotionShortfall {
    if evidence.sensitivity != crate::core::SensitivityClass::Normal
        && !evidence.strongest_explicitness.is_explicit()
    {
        return PromotionShortfall::RequiresExplicitStatement;
    }
    if evidence.evidence_count < config.minimum_evidence_count {
        return PromotionShortfall::Evidence;
    }
    if evidence.distinct_sessions < config.minimum_distinct_sessions {
        return PromotionShortfall::Sessions;
    }
    if evidence.distinct_days < config.minimum_distinct_days {
        return PromotionShortfall::Days;
    }
    PromotionShortfall::Confidence
}

/// Run a promotion sweep across every staged record in a namespace.
pub fn sweep(
    records: &[CanonicalMemory],
    config: &PromotionConfig,
    now: DateTime<Utc>,
) -> Vec<PromotionOutcome> {
    records
        .iter()
        .filter(|m| m.status == MemoryStatus::Staged)
        .map(|m| evaluate(m, config, now))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        CanonicalPredicate, EntityRef, EvidenceCounters, MemoryId, MemoryKind, MemorySource,
        MemoryValue, PrivacyMetadata, RetrievalMetadata, SensitivityClass, SessionId,
        TemporalMetadata, TemporalScope, TurnId, UserId,
    };

    fn staged(
        count: u32,
        sessions: u32,
        days: u32,
        confidence: f32,
        explicitness: Explicitness,
    ) -> CanonicalMemory {
        let now = Utc::now();
        CanonicalMemory {
            id: MemoryId::new("mem_staged"),
            owner: UserId::new("usr_1"),
            kind: MemoryKind::Routine,
            predicate: CanonicalPredicate::new("exercise_routine"),
            status: MemoryStatus::Staged,
            confidence,
            subject: EntityRef::user(),
            value: MemoryValue::Text("morning gym".into()),
            statement: "The user exercises before work.".into(),
            evidence_summary: "inferred".into(),
            source: MemorySource::from_explicitness(
                explicitness,
                SessionId::new("ses_1"),
                TurnId(2),
            ),
            temporal: TemporalMetadata::created_at(now),
            retrieval: RetrievalMetadata {
                subject: "user".into(),
                ..Default::default()
            },
            evidence: EvidenceCounters {
                count,
                distinct_sessions: sessions,
                distinct_days: days,
            },
            privacy: PrivacyMetadata::default(),
            temporal_scope: TemporalScope::Persistent,
            supersedes: Vec::new(),
            superseded_by: None,
            qualifier: None,
        }
    }

    #[test]
    fn a_pattern_seen_across_sessions_and_days_is_promoted() {
        let outcome = evaluate(
            &staged(3, 2, 2, 0.85, Explicitness::StrongImplication),
            &PromotionConfig::default(),
            Utc::now(),
        );
        match outcome {
            PromotionOutcome::Promote(memory) => {
                assert_eq!(memory.status, MemoryStatus::Active);
                assert!(memory.evidence_summary.contains("Promoted from a staged"));
            }
            other => panic!("expected promotion, got {other:?}"),
        }
    }

    #[test]
    fn repetition_within_one_session_is_held_not_promoted() {
        let outcome = evaluate(
            &staged(5, 1, 1, 0.9, Explicitness::StrongImplication),
            &PromotionConfig::default(),
            Utc::now(),
        );
        assert_eq!(
            outcome,
            PromotionOutcome::Hold {
                reason: PromotionShortfall::Sessions
            }
        );
    }

    #[test]
    fn each_shortfall_is_reported_specifically() {
        let config = PromotionConfig::default();
        let now = Utc::now();
        assert_eq!(
            evaluate(
                &staged(1, 2, 2, 0.9, Explicitness::StrongImplication),
                &config,
                now
            ),
            PromotionOutcome::Hold {
                reason: PromotionShortfall::Evidence
            }
        );
        assert_eq!(
            evaluate(
                &staged(3, 2, 1, 0.9, Explicitness::StrongImplication),
                &config,
                now
            ),
            PromotionOutcome::Hold {
                reason: PromotionShortfall::Days
            }
        );
        assert_eq!(
            evaluate(
                &staged(3, 2, 2, 0.5, Explicitness::StrongImplication),
                &config,
                now
            ),
            PromotionOutcome::Hold {
                reason: PromotionShortfall::Confidence
            }
        );
    }

    #[test]
    fn a_sensitive_pattern_is_never_promoted_on_repetition_alone() {
        let mut record = staged(9, 5, 5, 0.99, Explicitness::StrongImplication);
        record.privacy.sensitivity = SensitivityClass::Sensitive;
        assert_eq!(
            evaluate(&record, &PromotionConfig::default(), Utc::now()),
            PromotionOutcome::Hold {
                reason: PromotionShortfall::RequiresExplicitStatement
            }
        );
    }

    #[test]
    fn a_stale_unreinforced_pattern_expires() {
        let mut record = staged(1, 1, 1, 0.4, Explicitness::WeakInference);
        record.temporal.last_confirmed_at = Utc::now() - Duration::days(120);
        assert!(matches!(
            evaluate(&record, &PromotionConfig::default(), Utc::now()),
            PromotionOutcome::Expire(_)
        ));
    }

    #[test]
    fn the_sweep_ignores_records_that_are_not_staged() {
        let mut active = staged(9, 9, 9, 0.99, Explicitness::ExplicitStatement);
        active.status = MemoryStatus::Active;
        assert!(sweep(&[active], &PromotionConfig::default(), Utc::now()).is_empty());
    }
}
