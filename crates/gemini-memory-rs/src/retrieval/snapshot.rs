//! Prepared memory snapshots — the immutable unit the model consumes.
//!
//! A snapshot is frozen when a turn begins and never changes while that turn is
//! in flight. Without that, a slow retrieval landing mid-response could change
//! what the model "remembers" halfway through a sentence.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::bm25::MemoryOrigin;
use crate::core::{MemoryId, MemoryKind, PlanId, SnapshotId, TemporalScope, TurnId};

/// How long a prepared snapshot stays usable before it is considered stale.
pub const SNAPSHOT_TTL_SECONDS: i64 = 120;

/// One memory as it will be shown to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievedMemory {
    /// Which record this is, for provenance and explanation.
    pub memory_id: MemoryId,
    /// The sentence itself.
    pub statement: String,
    /// What sort of memory it is.
    pub kind: MemoryKind,
    /// How durable the fact is.
    pub temporal_scope: TemporalScope,
    /// Whether the fact is committed or still session-local.
    pub origin: MemoryOrigin,
    /// Fused retrieval score, retained for debugging.
    pub score: f32,
}

impl RetrievedMemory {
    /// The statement as it should be phrased to the model.
    ///
    /// An uncommitted session fact is hedged — "the user mentioned" rather than
    /// "the user prefers" — because it has not yet survived reconciliation and
    /// the model should not assert it as settled.
    pub fn presented_statement(&self) -> String {
        match self.origin {
            MemoryOrigin::Canonical => self.statement.clone(),
            MemoryOrigin::SessionOverlay => {
                let trimmed = self.statement.trim_end_matches('.');
                let lowered = lowercase_first(trimmed);
                format!("The user mentioned that {lowered}.")
            }
        }
    }

    /// Estimated token cost of the presented statement.
    pub fn token_cost(&self) -> usize {
        estimate_tokens(&self.presented_statement())
    }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Approximate the token cost of a string.
///
/// Deliberately an over-estimate: the budget exists to protect latency and
/// context, and being slightly conservative is cheaper than blowing the cap.
pub fn estimate_tokens(text: &str) -> usize {
    let words = text.split_whitespace().count();
    let by_chars = text.chars().count().div_ceil(4);
    words.max(by_chars).max(1)
}

/// An immutable, budgeted set of memories prepared for a turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreparedMemorySnapshot {
    /// Snapshot identity.
    pub snapshot_id: SnapshotId,
    /// Canonical repository revision the snapshot was built from.
    pub user_memory_revision: u64,
    /// Session overlay revision the snapshot was built from.
    pub session_overlay_revision: u64,
    /// The plan that produced it.
    pub retrieval_plan_id: Option<PlanId>,
    /// The turn whose transcript produced it.
    pub source_turn_id: TurnId,
    /// The first turn it may be served to.
    pub eligible_from_turn: TurnId,
    /// The memories, in presentation order.
    pub facts: Arc<[RetrievedMemory]>,
    /// Total estimated tokens.
    pub token_count: u16,
    /// Cache key of the plan that produced it.
    pub cache_key: String,
    /// When it was prepared.
    pub created_at: DateTime<Utc>,
    /// When it stops being usable.
    pub expires_at: DateTime<Utc>,
}

impl Default for PreparedMemorySnapshot {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            snapshot_id: SnapshotId::generate(),
            user_memory_revision: 0,
            session_overlay_revision: 0,
            retrieval_plan_id: None,
            source_turn_id: TurnId::ZERO,
            eligible_from_turn: TurnId::ZERO,
            facts: Arc::from(Vec::new()),
            token_count: 0,
            cache_key: String::new(),
            created_at: now,
            expires_at: now + Duration::seconds(SNAPSHOT_TTL_SECONDS),
        }
    }
}

impl PreparedMemorySnapshot {
    /// An empty snapshot for a turn.
    pub fn empty(turn_id: TurnId) -> Self {
        Self {
            source_turn_id: turn_id,
            eligible_from_turn: turn_id,
            ..Default::default()
        }
    }

    /// Whether the snapshot holds anything.
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }

    /// Whether the snapshot is still fresh at `now`.
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        now < self.expires_at
    }

    /// Whether this snapshot answers the query the model actually asked with.
    ///
    /// A prepared snapshot is speculative: it was built from the transcript,
    /// not from the tool arguments. When the model asks something the
    /// speculation did not anticipate, the caller falls back to a live search
    /// rather than answering the wrong question quickly.
    pub fn satisfies(&self, query: &str, now: DateTime<Utc>) -> bool {
        if !self.is_fresh(now) || self.facts.is_empty() {
            return false;
        }
        let query_terms = crate::bm25::tokenize(query);
        if query_terms.is_empty() {
            return true;
        }
        let covered: Vec<String> = self
            .facts
            .iter()
            .flat_map(|f| crate::bm25::tokenize(&f.statement))
            .collect();
        let overlap = query_terms.iter().filter(|t| covered.contains(t)).count();
        // At least a third of the asked-for terms should appear in what was
        // prepared, or the speculation missed.
        overlap * 3 >= query_terms.len()
    }

    /// The tool payload handed back to the model (§39.2).
    pub fn to_tool_payload(&self) -> serde_json::Value {
        if self.facts.is_empty() {
            return serde_json::json!({ "status": "not_found", "facts": [] });
        }
        serde_json::json!({
            "status": "found",
            "facts": self
                .facts
                .iter()
                .map(|f| serde_json::json!({
                    "statement": f.presented_statement(),
                    "kind": f.kind,
                    "temporal_scope": f.temporal_scope,
                }))
                .collect::<Vec<_>>(),
            "token_count": self.token_count,
        })
    }
}

/// Merge a live search with a prepared snapshot, ranked by RRF.
///
/// Serving one *or* the other is a choice the engine used to make with
/// [`PreparedMemorySnapshot::satisfies`], and it made it badly: measured
/// against snapshots that already held the answer, `satisfies` refused 65 of 93
/// paraphrased questions, discarding a correct snapshot in favour of a lexical
/// search that could not find one. Neither ranking is reliably better, so
/// neither gets to win outright.
///
/// The same `1/(60 + rank)` the retriever already fuses lexical rankings with,
/// so a fact both agree on rises and a fact only one found still gets a place.
/// A stale prepared snapshot contributes nothing — it is a snapshot of a
/// conversation that has moved on.
/// `max_tokens` is a hard cap, not a target. Both inputs were assembled under
/// it independently, which does *not* make their union compliant: two
/// single-fact snapshots of 400 tokens each fuse to 800 under a 500 cap. The
/// count limit alone cannot catch that — it is a limit on a different quantity
/// — so the budget is reapplied here, the same way [`ContextAssembler`] applies
/// it, and for the same reason: whatever this returns is what fills the model's
/// context.
///
/// [`ContextAssembler`]: crate::retrieval::ContextAssembler
pub fn fuse_snapshots(
    live: &PreparedMemorySnapshot,
    prepared: &PreparedMemorySnapshot,
    max_memories: usize,
    max_tokens: usize,
    now: DateTime<Utc>,
) -> PreparedMemorySnapshot {
    if prepared.is_empty() || !prepared.is_fresh(now) {
        return live.clone();
    }
    if live.is_empty() {
        return prepared.clone();
    }

    let k = crate::retrieval::fusion::RRF_K as f64;
    let mut scores: Vec<(f64, RetrievedMemory)> = Vec::new();
    for ranking in [&live.facts, &prepared.facts] {
        for (rank, fact) in ranking.iter().enumerate() {
            let contribution = 1.0 / (k + rank as f64 + 1.0);
            match scores
                .iter_mut()
                .find(|(_, f)| f.memory_id == fact.memory_id)
            {
                Some((score, _)) => *score += contribution,
                None => scores.push((contribution, fact.clone())),
            }
        }
    }
    scores.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scores.truncate(max_memories);

    // Fill in fused order until the next fact would break the budget, then
    // stop. `break` rather than `continue`: skipping an expensive fact to fit a
    // cheaper one behind it would serve a lower-ranked memory in place of a
    // higher-ranked one, which is the fusion silently disagreeing with itself.
    // Running out of budget is a truncation, not a re-ranking.
    let mut facts: Vec<RetrievedMemory> = Vec::with_capacity(scores.len());
    let mut tokens = 0usize;
    for (_, fact) in scores {
        let cost = fact.token_cost();
        if tokens + cost > max_tokens {
            break;
        }
        tokens += cost;
        facts.push(fact);
    }
    let token_count = tokens.min(u16::MAX as usize) as u16;
    PreparedMemorySnapshot {
        facts: Arc::from(facts),
        token_count,
        // Provenance follows the live search: it is the one that answered the
        // question the model actually asked.
        ..live.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(statement: &str, origin: MemoryOrigin) -> RetrievedMemory {
        RetrievedMemory {
            memory_id: MemoryId::new("mem_1"),
            statement: statement.to_string(),
            kind: MemoryKind::Preference,
            temporal_scope: TemporalScope::Persistent,
            origin,
            score: 3.0,
        }
    }

    fn snapshot(facts: Vec<RetrievedMemory>) -> PreparedMemorySnapshot {
        let token_count = facts.iter().map(|f| f.token_cost()).sum::<usize>() as u16;
        PreparedMemorySnapshot {
            facts: Arc::from(facts),
            token_count,
            ..Default::default()
        }
    }

    #[test]
    fn canonical_facts_are_asserted_and_overlay_facts_are_hedged() {
        assert_eq!(
            fact("The user is pescatarian.", MemoryOrigin::Canonical).presented_statement(),
            "The user is pescatarian."
        );
        assert_eq!(
            fact("The user is pescatarian.", MemoryOrigin::SessionOverlay).presented_statement(),
            "The user mentioned that the user is pescatarian."
        );
    }

    /// The cap `ContextAssembler` enforces has to survive fusion.
    ///
    /// Both inputs are individually legal — one fact each, comfortably under a
    /// generous cap — and their union is not. Before the budget was reapplied
    /// here, fusion truncated by *count* and then summed whatever it kept, so
    /// two compliant snapshots produced a non-compliant one and the guarantee
    /// the assembler makes stopped holding the moment speculation was involved.
    #[test]
    fn fusing_two_compliant_snapshots_does_not_exceed_the_token_cap() {
        let long = "The user has a standing preference that every dinner \
                    reservation be somewhere quiet enough to hold a conversation \
                    without raising a voice, ideally with outdoor seating.";
        let mut live_fact = fact(long, MemoryOrigin::Canonical);
        live_fact.memory_id = MemoryId::new("mem_live");
        let mut prepared_fact = fact(long, MemoryOrigin::Canonical);
        prepared_fact.memory_id = MemoryId::new("mem_prepared");

        let each = live_fact.token_cost();
        assert!(each > 0, "the fixture fact must cost something");
        // A cap that either fact fits inside and both together do not.
        let cap = each + each / 2;

        let fused = fuse_snapshots(
            &snapshot(vec![live_fact]),
            &snapshot(vec![prepared_fact]),
            10,
            cap,
            Utc::now(),
        );
        assert!(
            usize::from(fused.token_count) <= cap,
            "fused snapshot is {} tokens against a {cap}-token cap",
            fused.token_count
        );
        assert_eq!(
            fused.facts.len(),
            1,
            "the budget should have stopped at the first fact"
        );
        // The reported count has to match what is actually carried, or the cap
        // is enforced against a number nobody is serving.
        assert_eq!(
            usize::from(fused.token_count),
            fused
                .facts
                .iter()
                .map(RetrievedMemory::token_cost)
                .sum::<usize>(),
        );
    }

    /// A cap generous enough for both must not drop either — otherwise the fix
    /// above would "pass" by always truncating.
    #[test]
    fn a_cap_that_fits_both_keeps_both() {
        let mut live_fact = fact("The user drinks cortados.", MemoryOrigin::Canonical);
        live_fact.memory_id = MemoryId::new("mem_live");
        let mut prepared_fact = fact("Rhea prefers quiet places.", MemoryOrigin::Canonical);
        prepared_fact.memory_id = MemoryId::new("mem_prepared");

        let fused = fuse_snapshots(
            &snapshot(vec![live_fact]),
            &snapshot(vec![prepared_fact]),
            10,
            10_000,
            Utc::now(),
        );
        assert_eq!(fused.facts.len(), 2);
    }

    #[test]
    fn an_empty_snapshot_reports_not_found() {
        let payload = PreparedMemorySnapshot::empty(TurnId(1)).to_tool_payload();
        assert_eq!(payload["status"], "not_found");
        assert_eq!(payload["facts"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn the_payload_carries_statements_kinds_and_a_token_count() {
        let payload = snapshot(vec![fact(
            "The user is pescatarian.",
            MemoryOrigin::Canonical,
        )])
        .to_tool_payload();
        assert_eq!(payload["status"], "found");
        assert_eq!(payload["facts"][0]["statement"], "The user is pescatarian.");
        assert_eq!(payload["facts"][0]["kind"], "preference");
        assert_eq!(payload["facts"][0]["temporal_scope"], "persistent");
        assert!(payload["token_count"].as_u64().unwrap() > 0);
    }

    #[test]
    fn a_snapshot_satisfies_a_query_it_actually_covers() {
        let snap = snapshot(vec![fact(
            "Rhea prefers quiet restaurants.",
            MemoryOrigin::Canonical,
        )]);
        let now = Utc::now();
        assert!(snap.satisfies("quiet restaurants for Rhea", now));
        assert!(!snap.satisfies("what medication does the user take", now));
    }

    #[test]
    fn a_stale_or_empty_snapshot_satisfies_nothing() {
        let now = Utc::now();
        let mut stale = snapshot(vec![fact("Rhea prefers quiet.", MemoryOrigin::Canonical)]);
        stale.expires_at = now - Duration::seconds(1);
        assert!(!stale.satisfies("quiet", now));
        assert!(!PreparedMemorySnapshot::empty(TurnId(1)).satisfies("anything", now));
    }

    #[test]
    fn token_estimates_are_conservative_and_never_zero() {
        assert!(estimate_tokens("The user is pescatarian.") >= 5);
        assert_eq!(estimate_tokens(""), 1);
    }
}
