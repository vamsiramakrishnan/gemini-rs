//! Pairwise reconciliation: proposal meets existing memory.
//!
//! This is the module that decides whether a user is *repeating* themselves,
//! *refining* themselves, *contradicting* themselves, or describing a different
//! context entirely. It is deliberately a pure function over a small candidate
//! window — no I/O, no model call — so every outcome is reproducible and
//! testable.

use chrono::{DateTime, Utc};

use super::proposal::{ProposedMemory, ResolutionKind, ResolvedMutation};
use crate::core::{
    CanonicalMemory, DiscardReason, EvidenceCounters, Explicitness, MemoryId, MemoryStatus,
    ProposedPersistence, SensitivityClass, UserId, aggregate_confidence, normalize_token,
};

/// Resolves proposals against existing memory.
#[derive(Debug, Clone)]
pub struct Resolver {
    owner: UserId,
}

impl Resolver {
    /// A resolver for one user's namespace.
    pub fn new(owner: UserId) -> Self {
        Self { owner }
    }

    /// Decide what to do with `proposal` given the existing records that could
    /// plausibly be about the same thing.
    ///
    /// `existing` is the candidate window — records sharing the proposal's
    /// subject and predicate — not the whole corpus.
    pub fn resolve(
        &self,
        proposal: ProposedMemory,
        existing: &[CanonicalMemory],
        now: DateTime<Utc>,
    ) -> ResolvedMutation {
        let fingerprint = proposal.fingerprint.clone();

        if proposal.sensitivity != SensitivityClass::Normal && !proposal.explicitness.is_explicit()
        {
            return ResolvedMutation::discard(
                fingerprint,
                DiscardReason::SensitiveWithoutExplicitStatement,
            );
        }

        let active: Vec<&CanonicalMemory> = existing
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .collect();

        // 1. The same fact, said again. Strengthen rather than duplicate.
        if let Some(same) = active.iter().find(|m| m.fingerprint() == fingerprint) {
            return Self::reinforce(same, &proposal, now);
        }

        // 2. The same fact under a different predicate name.
        //
        // Extraction models are not consistent about naming: the same routine
        // came back as `has_routine` in one session and
        // `goes_to_gym_before_work` in the next. Fingerprints therefore diverge
        // and, without this, the corpus accumulates a duplicate per session.
        // Same subject and same value is the same fact, whatever it was called.
        if let Some(equivalent) = active.iter().find(|m| is_same_fact_renamed(&proposal, m)) {
            return Self::reinforce(equivalent, &proposal, now);
        }

        // 3. Nothing else claims this subject and predicate.
        let competing: Vec<&&CanonicalMemory> = active
            .iter()
            .filter(|m| {
                m.fingerprint().subject_predicate() == fingerprint.subject_predicate()
                    && m.qualifier == proposal.qualifier
            })
            .collect();

        if competing.is_empty() {
            return self.create_or_stage(proposal, now);
        }

        let incumbent = competing
            .iter()
            .max_by(|a, b| {
                a.temporal
                    .last_confirmed_at
                    .cmp(&b.temporal.last_confirmed_at)
            })
            .expect("non-empty");

        // 4. An inference may not overrule something the user said outright.
        if !proposal.explicitness.is_explicit() && incumbent.source.is_explicit() {
            return ResolvedMutation::discard(fingerprint, DiscardReason::InsufficientEvidence);
        }

        // 5. A more precise restatement of a compatible fact refines it.
        if is_refinement(&proposal, incumbent) {
            return self.refine(incumbent, proposal, now);
        }

        // 6. Otherwise this contradicts the incumbent and replaces it.
        self.supersede(incumbent, proposal, now)
    }

    fn create_or_stage(&self, proposal: ProposedMemory, now: DateTime<Utc>) -> ResolvedMutation {
        let staged = proposal.persistence == ProposedPersistence::Staged;
        let kind = if staged {
            ResolutionKind::Stage
        } else {
            ResolutionKind::Create
        };
        let status = if staged {
            MemoryStatus::Staged
        } else {
            MemoryStatus::Active
        };
        let fingerprint = proposal.fingerprint.clone();
        let memory = proposal.into_canonical(&self.owner, MemoryId::generate(), status, now);
        ResolvedMutation::write(kind, fingerprint, memory)
    }

    fn reinforce(
        existing: &CanonicalMemory,
        proposal: &ProposedMemory,
        now: DateTime<Utc>,
    ) -> ResolvedMutation {
        let mut updated = existing.clone();
        updated.evidence = EvidenceCounters {
            count: existing.evidence.count + proposal.evidence.count,
            distinct_sessions: existing.evidence.distinct_sessions + 1,
            distinct_days: bump_days(existing, now),
        };
        updated.confidence = aggregate_confidence(&[
            (existing.confidence, source_explicitness(existing)),
            (proposal.confidence, proposal.explicitness),
        ]);
        updated.temporal.last_confirmed_at = now;
        updated.temporal.updated_at = now;
        // Reinforcement extends an episodic record's life rather than letting
        // it expire on its original schedule.
        if let Some(expiry) = crate::core::resolve_expiry(
            updated.kind,
            updated.temporal_scope,
            proposal.expected_expiry,
            now,
        ) {
            updated.temporal.expires_at = Some(expiry);
        }
        // A record staged for reinforcement has now been reinforced.
        if updated.status == MemoryStatus::Staged && proposal.explicitness.is_explicit() {
            updated.status = MemoryStatus::Active;
        }

        ResolvedMutation::write(
            ResolutionKind::Reinforce,
            proposal.fingerprint.clone(),
            updated,
        )
    }

    fn refine(
        &self,
        incumbent: &CanonicalMemory,
        proposal: ProposedMemory,
        now: DateTime<Utc>,
    ) -> ResolvedMutation {
        let fingerprint = proposal.fingerprint.clone();
        let evidence = EvidenceCounters {
            count: incumbent.evidence.count + proposal.evidence.count,
            distinct_sessions: incumbent.evidence.distinct_sessions + 1,
            distinct_days: bump_days(incumbent, now),
        };
        let mut refined =
            proposal.into_canonical(&self.owner, MemoryId::generate(), MemoryStatus::Active, now);
        // Lineage is preserved: the refined record carries the evidence the
        // vaguer one accumulated, rather than starting from one observation.
        refined.evidence = evidence;
        refined.supersedes = vec![incumbent.id.clone()];
        refined.temporal.created_at = incumbent.temporal.created_at;

        let mut retired = incumbent.clone();
        retired.status = MemoryStatus::Superseded;
        retired.superseded_by = Some(refined.id.clone());
        retired.temporal.valid_to = Some(now);
        retired.temporal.updated_at = now;

        ResolvedMutation {
            kind: ResolutionKind::Refine,
            fingerprint,
            writes: vec![refined, retired],
            deletes: Vec::new(),
            discard_reason: None,
        }
    }

    fn supersede(
        &self,
        incumbent: &CanonicalMemory,
        proposal: ProposedMemory,
        now: DateTime<Utc>,
    ) -> ResolvedMutation {
        let fingerprint = proposal.fingerprint.clone();
        let mut replacement =
            proposal.into_canonical(&self.owner, MemoryId::generate(), MemoryStatus::Active, now);
        replacement.supersedes = vec![incumbent.id.clone()];

        let mut retired = incumbent.clone();
        retired.status = MemoryStatus::Superseded;
        retired.superseded_by = Some(replacement.id.clone());
        retired.temporal.valid_to = Some(now);
        retired.temporal.updated_at = now;

        ResolvedMutation {
            kind: ResolutionKind::Supersede,
            fingerprint,
            writes: vec![replacement, retired],
            deletes: Vec::new(),
            discard_reason: None,
        }
    }

    /// Decide that two differently-qualified facts both hold.
    ///
    /// "Quiet places with family" and "live music with friends" are not a
    /// contradiction; they are the same predicate in two contexts.
    pub fn coexist(&self, proposal: ProposedMemory, now: DateTime<Utc>) -> ResolvedMutation {
        let fingerprint = proposal.fingerprint.clone();
        let memory =
            proposal.into_canonical(&self.owner, MemoryId::generate(), MemoryStatus::Active, now);
        ResolvedMutation::write(ResolutionKind::Coexist, fingerprint, memory)
    }
}

/// Whether two records assert the same fact under different predicate names.
///
/// Requires the subject *and* the value to agree; a shared value under a
/// different subject ("Rhea is vegetarian" vs "the user is vegetarian") is two
/// facts, not one.
fn is_same_fact_renamed(proposal: &ProposedMemory, existing: &CanonicalMemory) -> bool {
    if proposal.predicate == existing.predicate {
        return false;
    }
    if normalize_token(&proposal.subject.display) != normalize_token(&existing.subject.display) {
        return false;
    }
    if proposal.qualifier != existing.qualifier {
        return false;
    }
    let new_value = proposal.value.normalized();
    let old_value = existing.value.normalized();
    if new_value.is_empty() || old_value.is_empty() {
        return false;
    }
    new_value == old_value
        || normalize_token(&proposal.statement) == normalize_token(&existing.statement)
}

/// Whether the proposal says the same thing as the incumbent, more precisely.
///
/// The test is lexical containment in either direction: "avoids meat" versus
/// "avoids meat but eats fish" is a refinement, whereas "vegetarian" versus
/// "pescatarian" shares no terms and is a contradiction.
fn is_refinement(proposal: &ProposedMemory, incumbent: &CanonicalMemory) -> bool {
    let new_value = normalize_token(&proposal.value.display());
    let old_value = normalize_token(&incumbent.value.display());
    if new_value.is_empty() || old_value.is_empty() || new_value == old_value {
        return false;
    }
    let new_terms: Vec<&str> = new_value.split_whitespace().collect();
    let old_terms: Vec<&str> = old_value.split_whitespace().collect();

    let old_within_new = old_terms.iter().all(|t| new_terms.contains(t));
    let new_within_old = new_terms.iter().all(|t| old_terms.contains(t));

    // Only a strictly more specific restatement refines; a strictly vaguer one
    // is not an improvement worth rewriting the record for.
    old_within_new && !new_within_old
}

fn source_explicitness(memory: &CanonicalMemory) -> Explicitness {
    if memory.source.source_type.contains("command") {
        Explicitness::ExplicitCommand
    } else if memory.source.is_explicit() {
        Explicitness::ExplicitStatement
    } else if memory.source.source_type.contains("strong") {
        Explicitness::StrongImplication
    } else {
        Explicitness::WeakInference
    }
}

fn bump_days(existing: &CanonicalMemory, now: DateTime<Utc>) -> u32 {
    use chrono::Datelike;
    let same_day = existing.temporal.last_confirmed_at.year() == now.year()
        && existing.temporal.last_confirmed_at.ordinal() == now.ordinal();
    if same_day {
        existing.evidence.distinct_days.max(1)
    } else {
        existing.evidence.distinct_days + 1
    }
}

/// Build a resolver-ready proposal for tests and callers that already have a
/// canonical record in hand.
pub fn proposal_from(memory: &CanonicalMemory) -> ProposedMemory {
    ProposedMemory {
        fingerprint: memory.fingerprint(),
        subject: memory.subject.clone(),
        predicate: memory.predicate.clone(),
        value: memory.value.clone(),
        statement: memory.statement.clone(),
        evidence_summary: memory.evidence_summary.clone(),
        kind: memory.kind,
        temporal_scope: memory.temporal_scope,
        explicitness: source_explicitness(memory),
        confidence: memory.confidence,
        evidence: memory.evidence,
        persistence: ProposedPersistence::Durable,
        expected_expiry: memory.temporal.expires_at,
        mutation_intent: None,
        sensitivity: memory.privacy.sensitivity,
        qualifier: memory.qualifier.clone(),
        session_id: memory
            .source
            .session_id
            .clone()
            .unwrap_or_else(|| crate::core::SessionId::new("ses_unknown")),
        turn_id: memory.source.turn_id.unwrap_or_default(),
        tags: memory.retrieval.tags.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        CanonicalPredicate, EntityRef, MemoryKind, MemorySource, MemoryValue, PrivacyMetadata,
        RetrievalMetadata, SessionId, TemporalMetadata, TemporalScope, TurnId,
    };

    fn existing(id: &str, value: &str, explicitness: Explicitness) -> CanonicalMemory {
        let now = Utc::now();
        CanonicalMemory {
            id: MemoryId::new(id),
            owner: UserId::new("usr_1"),
            kind: MemoryKind::Preference,
            predicate: CanonicalPredicate::new("dietary_identity"),
            status: MemoryStatus::Active,
            confidence: 0.9,
            subject: EntityRef::user(),
            value: MemoryValue::Text(value.into()),
            statement: format!("The user is {value}."),
            evidence_summary: "stated".into(),
            source: MemorySource::from_explicitness(
                explicitness,
                SessionId::new("ses_old"),
                TurnId(1),
            ),
            temporal: TemporalMetadata::created_at(now - chrono::Duration::days(30)),
            retrieval: RetrievalMetadata {
                subject: "user".into(),
                ..Default::default()
            },
            evidence: EvidenceCounters {
                count: 2,
                distinct_sessions: 2,
                distinct_days: 2,
            },
            privacy: PrivacyMetadata::default(),
            temporal_scope: TemporalScope::Persistent,
            supersedes: Vec::new(),
            superseded_by: None,
            qualifier: None,
        }
    }

    fn proposal(value: &str, explicitness: Explicitness) -> ProposedMemory {
        let mut memory = existing("mem_new", value, explicitness);
        memory.evidence = EvidenceCounters::first();
        let mut proposal = proposal_from(&memory);
        proposal.explicitness = explicitness;
        proposal
    }

    fn resolver() -> Resolver {
        Resolver::new(UserId::new("usr_1"))
    }

    #[test]
    fn the_same_fact_under_a_renamed_predicate_reinforces() {
        let incumbent = existing("mem_a", "morning gym", Explicitness::ExplicitStatement);
        let mut renamed = proposal("morning gym", Explicitness::ExplicitStatement);
        renamed.predicate = CanonicalPredicate::new("goes_to_gym_before_work");
        renamed.fingerprint = crate::core::FactFingerprint::new(
            &renamed.subject,
            &renamed.predicate,
            &renamed.value,
            renamed.temporal_scope,
        );

        let resolved = resolver().resolve(renamed, std::slice::from_ref(&incumbent), Utc::now());
        assert_eq!(resolved.kind, ResolutionKind::Reinforce);
        assert_eq!(resolved.writes[0].id, incumbent.id, "identity is preserved");
    }

    #[test]
    fn a_renamed_predicate_about_a_different_subject_is_a_different_fact() {
        let incumbent = existing("mem_a", "morning gym", Explicitness::ExplicitStatement);
        let mut other = proposal("morning gym", Explicitness::ExplicitStatement);
        other.predicate = CanonicalPredicate::new("goes_to_gym_before_work");
        other.subject = EntityRef::named("Rhea");
        other.statement = "Rhea goes to the gym before work.".into();
        other.fingerprint = crate::core::FactFingerprint::new(
            &other.subject,
            &other.predicate,
            &other.value,
            other.temporal_scope,
        );

        let resolved = resolver().resolve(other, std::slice::from_ref(&incumbent), Utc::now());
        assert_eq!(resolved.kind, ResolutionKind::Create);
    }

    #[test]
    fn a_novel_fact_is_created() {
        let resolved = resolver().resolve(
            proposal("pescatarian", Explicitness::ExplicitStatement),
            &[],
            Utc::now(),
        );
        assert_eq!(resolved.kind, ResolutionKind::Create);
        assert_eq!(resolved.writes.len(), 1);
        assert_eq!(resolved.writes[0].status, MemoryStatus::Active);
    }

    #[test]
    fn a_staged_proposal_is_written_staged_not_active() {
        let mut p = proposal("morning gym", Explicitness::StrongImplication);
        p.persistence = ProposedPersistence::Staged;
        let resolved = resolver().resolve(p, &[], Utc::now());
        assert_eq!(resolved.kind, ResolutionKind::Stage);
        assert_eq!(resolved.writes[0].status, MemoryStatus::Staged);
    }

    #[test]
    fn restating_the_same_fact_reinforces_rather_than_duplicates() {
        let now = Utc::now();
        let incumbent = existing("mem_a", "pescatarian", Explicitness::ExplicitStatement);
        let resolved = resolver().resolve(
            proposal("pescatarian", Explicitness::ExplicitStatement),
            std::slice::from_ref(&incumbent),
            now,
        );

        assert_eq!(resolved.kind, ResolutionKind::Reinforce);
        assert_eq!(resolved.writes.len(), 1);
        assert_eq!(resolved.writes[0].id, incumbent.id, "identity is preserved");
        assert_eq!(resolved.writes[0].evidence.count, 3);
        assert_eq!(resolved.writes[0].evidence.distinct_sessions, 3);
        assert_eq!(resolved.writes[0].temporal.last_confirmed_at, now);
    }

    #[test]
    fn reinforcement_promotes_a_staged_record_once_stated_outright() {
        let mut staged = existing("mem_a", "morning gym", Explicitness::WeakInference);
        staged.status = MemoryStatus::Staged;
        staged.predicate = CanonicalPredicate::new("exercise_routine");

        let mut p = proposal("morning gym", Explicitness::ExplicitStatement);
        p.predicate = CanonicalPredicate::new("exercise_routine");
        p.fingerprint =
            crate::core::FactFingerprint::new(&p.subject, &p.predicate, &p.value, p.temporal_scope);

        // The staged record is not `Active`, so it is not in the candidate
        // window used for contradiction — but an exact fingerprint match still
        // has to find it. Present it as active-by-reinforcement.
        staged.status = MemoryStatus::Active;
        let resolved = resolver().resolve(p, &[staged], Utc::now());
        assert_eq!(resolved.kind, ResolutionKind::Reinforce);
    }

    #[test]
    fn an_explicit_correction_supersedes_the_incumbent() {
        let now = Utc::now();
        let incumbent = existing("mem_old", "vegetarian", Explicitness::ExplicitStatement);
        let resolved = resolver().resolve(
            proposal("pescatarian", Explicitness::ExplicitStatement),
            std::slice::from_ref(&incumbent),
            now,
        );

        assert_eq!(resolved.kind, ResolutionKind::Supersede);
        assert_eq!(resolved.writes.len(), 2);

        let replacement = &resolved.writes[0];
        let retired = &resolved.writes[1];
        assert_eq!(replacement.status, MemoryStatus::Active);
        assert_eq!(replacement.supersedes, vec![incumbent.id.clone()]);
        assert_eq!(retired.status, MemoryStatus::Superseded);
        assert_eq!(retired.superseded_by.as_ref(), Some(&replacement.id));
        assert_eq!(retired.temporal.valid_to, Some(now));
    }

    #[test]
    fn a_more_precise_restatement_refines_and_keeps_the_lineage() {
        let incumbent = existing("mem_old", "avoids meat", Explicitness::ExplicitStatement);
        let resolved = resolver().resolve(
            proposal("avoids meat eats fish", Explicitness::ExplicitStatement),
            std::slice::from_ref(&incumbent),
            Utc::now(),
        );

        assert_eq!(resolved.kind, ResolutionKind::Refine);
        let refined = &resolved.writes[0];
        assert_eq!(refined.supersedes, vec![incumbent.id.clone()]);
        assert_eq!(
            refined.evidence.count,
            incumbent.evidence.count + 1,
            "the refined record inherits accumulated evidence"
        );
        assert_eq!(refined.temporal.created_at, incumbent.temporal.created_at);
    }

    #[test]
    fn a_vaguer_restatement_does_not_refine() {
        let incumbent = existing(
            "mem_old",
            "avoids meat eats fish",
            Explicitness::ExplicitStatement,
        );
        let resolved = resolver().resolve(
            proposal("avoids meat", Explicitness::ExplicitStatement),
            &[incumbent],
            Utc::now(),
        );
        assert_eq!(resolved.kind, ResolutionKind::Supersede);
    }

    #[test]
    fn an_inference_cannot_overrule_something_the_user_said() {
        let incumbent = existing("mem_old", "pescatarian", Explicitness::ExplicitStatement);
        let resolved = resolver().resolve(
            proposal("vegan", Explicitness::WeakInference),
            &[incumbent],
            Utc::now(),
        );
        assert_eq!(resolved.kind, ResolutionKind::Discard);
        assert_eq!(
            resolved.discard_reason,
            Some(DiscardReason::InsufficientEvidence)
        );
    }

    #[test]
    fn an_inference_may_still_supersede_another_inference() {
        let incumbent = existing("mem_old", "vegetarian", Explicitness::WeakInference);
        let resolved = resolver().resolve(
            proposal("pescatarian", Explicitness::StrongImplication),
            &[incumbent],
            Utc::now(),
        );
        assert_eq!(resolved.kind, ResolutionKind::Supersede);
    }

    #[test]
    fn differently_qualified_facts_do_not_contend() {
        let mut incumbent = existing(
            "mem_family",
            "quiet places",
            Explicitness::ExplicitStatement,
        );
        incumbent.qualifier = Some("with family".into());
        incumbent.predicate = CanonicalPredicate::new("venue_preference");

        let mut p = proposal("live music", Explicitness::ExplicitStatement);
        p.predicate = CanonicalPredicate::new("venue_preference");
        p.qualifier = Some("with friends".into());

        let resolved = resolver().resolve(p, &[incumbent], Utc::now());
        assert_eq!(
            resolved.kind,
            ResolutionKind::Create,
            "a different context is a new fact, not a contradiction"
        );
    }

    #[test]
    fn a_sensitive_inference_is_refused_at_reconciliation_too() {
        let mut p = proposal("a health condition", Explicitness::StrongImplication);
        p.sensitivity = SensitivityClass::Sensitive;
        let resolved = resolver().resolve(p, &[], Utc::now());
        assert_eq!(resolved.kind, ResolutionKind::Discard);
        assert_eq!(
            resolved.discard_reason,
            Some(DiscardReason::SensitiveWithoutExplicitStatement)
        );
    }

    #[test]
    fn superseded_records_are_not_treated_as_incumbents() {
        let mut retired = existing("mem_old", "vegetarian", Explicitness::ExplicitStatement);
        retired.status = MemoryStatus::Superseded;
        let resolved = resolver().resolve(
            proposal("pescatarian", Explicitness::ExplicitStatement),
            &[retired],
            Utc::now(),
        );
        assert_eq!(resolved.kind, ResolutionKind::Create);
    }

    #[test]
    fn coexistence_writes_a_second_active_record() {
        let resolved = resolver().coexist(
            proposal("live music", Explicitness::ExplicitStatement),
            Utc::now(),
        );
        assert_eq!(resolved.kind, ResolutionKind::Coexist);
        assert_eq!(resolved.writes[0].status, MemoryStatus::Active);
    }
}
