//! Whether a semantic layer would ever reach the model, measured before one is
//! built.
//!
//! # Why this exists
//!
//! `semantic_fusion_probe` establishes that embedding the record's own
//! frontmatter answers 66 of 93 paraphrased questions against BM25's 42. That
//! measures an *index*. It says nothing about whether the engine would ever
//! show the model what that index found, and the engine had five separate
//! places where it would not.
//!
//! # The method
//!
//! Every measurement here hands the seam a **perfect** semantic backend — an
//! oracle that returns the right record first, every time. That is deliberate.
//! A real backend that scores badly and a perfect backend the plumbing throws
//! away look identical from the outside, and only the second is worth fixing
//! before the first is built. If the oracle cannot get an answer to the model,
//! no embedding model can.
//!
//! Everything is deterministic and local: no API key, no network, no model.
//!
//! # What it found, and what changed
//!
//! With a perfect backend installed, over 93 questions and 1,199 records:
//!
//! | | before | after |
//! |---|---|---|
//! | answered on the `recall_context` path | 58/93 | **93/93** |
//! | answered on the speculative path | 71/93 | **93/93** |
//! | times the seam consulted the backend | 13 of 186 | 186 of 186 |
//!
//! 58 is exactly what BM25 answers alone: the tool path was not degrading the
//! semantic backend, it was never once asking it. Five causes, each fixed by
//! the commit this file accompanies:
//!
//! 1. `MemoryEngine::begin_session` built `LocalMemoryRetriever::new` directly,
//!    so `with_semantic_fallback` could not be called on the retriever a session
//!    actually uses — no application could install a backend at all. There is
//!    now `MemoryEngine::with_semantic_fallback`.
//! 2. `RetrievalBudget::interactive()` set `semantic_ms: 0`. The reasoning was
//!    sound for a remote backend and wrong for a local one, so it is now a
//!    deadline (`immediate_semantic_timeout_ms`, 10 ms) rather than a
//!    prohibition: a local scan replies inside it, a network call times out and
//!    the lexical results stand, which is what the zero achieved anyway.
//! 3. `needs_semantic_fallback` asked the backend only when lexical search came
//!    back thin — 13 of 93 questions. All 13 were rescued; the 80 it declined
//!    were declined because BM25 was *confident*, which is the failure mode
//!    rather than the safe case. The gate is gone.
//! 4. Semantic hits were appended below every lexical hit, "a safety net, not a
//!    competing opinion". The opinion is measurably the better one, so it is now
//!    fused as a ranking at 2:1 — the weighting `semantic_fusion_probe`
//!    measured at 79/93 in the top five.
//! 5. `recall` served the prepared snapshot *or* a live search, choosing with
//!    [`PreparedMemorySnapshot::satisfies`] — a lexical overlap test that
//!    refuses 65 of 93 snapshots **that already contain the answer**, and
//!    refuses hardest exactly where speculation is most valuable (0 of 6 asked
//!    in-situ, 1 of 20 needing inference). A refusal now demotes the snapshot to
//!    one ranking of two instead of discarding it.
//!
//! The 93/93 is the *plumbing* being transparent, not a claim about retrieval
//! quality: an oracle is not available in production. What it means is that
//! whatever a real backend knows now reaches the model, so the remaining
//! quality question belongs entirely to the backend. The `gate lets in` column
//! stays in the report as a live measurement of how often the fast path fires,
//! which is a latency question rather than a correctness one now.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::corpus::{self, PROBES, payload_statements, says_any};
use common::paraphrase::{self, Mode, Tier};
use common::{ScratchDir, file_backed_engine};

use async_trait::async_trait;
use chrono::Utc;
use gemini_memory_rs::bm25::{IndexedMemory, MemoryIndex, MemoryOrigin};
use gemini_memory_rs::core::{
    CanonicalMemory, MemoryError, MemoryId, MemoryStatus, RetrievalConfig, SessionId,
    TemporalScope, TurnId,
};
use gemini_memory_rs::retrieval::{
    IndexHandle, LocalMemoryRetriever, MemoryRetriever, PreparedMemorySnapshot, RetrievalBudget,
    RetrievedMemory, SemanticFallback,
};

// ─── the oracle ─────────────────────────────────────────────────────────────

/// A semantic backend that is always right.
///
/// Stands in for the ceiling of any embedding model: if the seam cannot deliver
/// *this*, the seam is what needs fixing, not the model.
struct Oracle {
    answers: HashMap<String, MemoryId>,
    /// How many times the seam actually asked. Zero is the finding.
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl SemanticFallback for Oracle {
    async fn search(&self, query: &str, _limit: usize) -> Result<Vec<MemoryId>, MemoryError> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self
            .answers
            .get(query)
            .map(|id| vec![id.clone()])
            .unwrap_or_default())
    }
}

impl Oracle {
    fn new() -> Arc<Self> {
        let mut answers = HashMap::new();
        for (probe_name, phrasing) in paraphrase::all() {
            let probe = probe(probe_name);
            answers.insert(phrasing.query.to_string(), MemoryId::new(probe.target));
        }
        Arc::new(Self {
            answers,
            calls: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::Relaxed)
    }
}

fn probe(name: &str) -> &'static corpus::Probe {
    PROBES
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("no probe named {name}"))
}

// ─── tallies ────────────────────────────────────────────────────────────────

#[derive(Default, Clone, Copy)]
struct Tally {
    asked: usize,
    passed: usize,
}

impl Tally {
    fn record(&mut self, passed: bool) {
        self.asked += 1;
        self.passed += usize::from(passed);
    }
    fn cell(&self) -> String {
        format!("{}/{}", self.passed, self.asked)
    }
}

/// One row of the report: the same question counted under each condition.
#[derive(Default, Clone, Copy)]
struct Row {
    gate: Tally,
    lexical_only: Tally,
    oracle_interactive: Tally,
    oracle_speculative: Tally,
}

impl Row {
    fn line(&self, label: &str) -> String {
        format!(
            "{:<15} {:<6} {:<12} {:<13} {:<18} {}\n",
            label,
            self.gate.asked,
            self.gate.cell(),
            self.lexical_only.cell(),
            self.oracle_interactive.cell(),
            self.oracle_speculative.cell(),
        )
    }
}

const HEADER: &str =
    "kind            asked  gate lets in  lexical top5  oracle+interactive  oracle+speculative\n";

// ─── fixtures ───────────────────────────────────────────────────────────────

/// The corpus, and a retriever over it, optionally with the oracle attached.
async fn corpus_records() -> Vec<CanonicalMemory> {
    let scratch = ScratchDir::new("speculation-gate");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    corpus::installed(&engine).await
}

fn retriever_over(
    records: &[CanonicalMemory],
    semantic: Option<Arc<Oracle>>,
) -> LocalMemoryRetriever {
    let active: Vec<&CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();
    let index = MemoryIndex::build(active.iter().map(|m| IndexedMemory::from_canonical(m)));
    let retriever = LocalMemoryRetriever::new(
        Arc::new(IndexHandle::with_index(index)),
        Arc::new(IndexHandle::new()),
        RetrievalConfig::default(),
    );
    match semantic {
        Some(oracle) => retriever.with_semantic_fallback(oracle),
        None => retriever,
    }
}

/// A snapshot shaped like a *successful* speculation: the right record, plus
/// filler to the five facts `max_memories` allows.
///
/// The filler matters. `satisfies` counts overlap against every statement in
/// the snapshot, so a one-fact snapshot would understate how often the gate
/// opens. Five facts is what the model would really be holding.
fn snapshot_holding(
    target: &CanonicalMemory,
    filler: &[&CanonicalMemory],
) -> PreparedMemorySnapshot {
    let facts: Vec<RetrievedMemory> = std::iter::once(target)
        .chain(filler.iter().copied())
        .map(|m| RetrievedMemory {
            memory_id: m.id.clone(),
            statement: m.statement.clone(),
            kind: m.kind,
            temporal_scope: TemporalScope::Persistent,
            origin: MemoryOrigin::Canonical,
            score: 3.0,
        })
        .collect();
    PreparedMemorySnapshot {
        facts: Arc::from(facts),
        ..Default::default()
    }
}

fn holds(snapshot: &PreparedMemorySnapshot, target: &str) -> bool {
    snapshot
        .facts
        .iter()
        .any(|f| f.memory_id.as_str() == target)
}

// ─── the measurement ────────────────────────────────────────────────────────

/// Four conditions over the same 93 questions, reported by tier and by mode.
#[tokio::test]
async fn how_often_a_perfect_semantic_layer_would_reach_the_model() {
    let records = corpus_records().await;
    let active: Vec<&CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();

    let oracle = Oracle::new();
    let plain = retriever_over(&records, None);
    let with_oracle = retriever_over(&records, Some(oracle.clone()));

    let mut by_tier: Vec<Row> = vec![Row::default(); Tier::COUNT];
    let mut by_mode: Vec<Row> = vec![Row::default(); Mode::COUNT];
    let mut overall = Row::default();
    let mut gate_refused: Vec<String> = Vec::new();
    let now = Utc::now();

    for (probe_name, phrasing) in paraphrase::all() {
        let probe = probe(probe_name);
        let query = phrasing.query;
        let target = corpus::by_id(&records, probe.target).expect("target in corpus");

        // ── 1. the gate, handed a snapshot that already contains the answer ──
        let filler: Vec<&CanonicalMemory> = active
            .iter()
            .copied()
            .filter(|m| m.id != target.id)
            .take(4)
            .collect();
        let perfect = snapshot_holding(target, &filler);
        let gate_opens = perfect.satisfies(query, now);

        // ── 2-4. the seam, with and without a perfect backend ──
        let lexical = plain
            .retrieve_immediate(query, TurnId(1), RetrievalBudget::interactive())
            .await
            .expect("lexical retrieval");
        let interactive = with_oracle
            .retrieve_immediate(query, TurnId(1), RetrievalBudget::interactive())
            .await
            .expect("oracle retrieval, interactive budget");
        let speculative = with_oracle
            .retrieve_immediate(query, TurnId(1), RetrievalBudget::speculative())
            .await
            .expect("oracle retrieval, speculative budget");

        for row in [
            &mut by_tier[phrasing.tier.index()],
            &mut by_mode[phrasing.mode.index()],
            &mut overall,
        ] {
            row.gate.record(gate_opens);
            row.lexical_only.record(holds(&lexical, probe.target));
            row.oracle_interactive
                .record(holds(&interactive, probe.target));
            row.oracle_speculative
                .record(holds(&speculative, probe.target));
        }

        if !gate_opens {
            gate_refused.push(format!(
                "[{}/{}] {query:?}",
                phrasing.tier.label(),
                phrasing.mode.label()
            ));
        }
    }

    let asked = paraphrase::count();
    let mut report = format!(
        "\nwould a semantic layer reach the model? {asked} questions, {} records\n\n\
         gate lets in         — a snapshot already holding the answer passes `satisfies`\n\
         lexical top5         — BM25 alone, the engine as it ships\n\
         oracle+interactive   — a perfect semantic backend, on the `recall_context` path\n\
         oracle+speculative   — the same backend, on the speculative path\n\n{HEADER}",
        active.len(),
    );
    for tier in Tier::ALL {
        report.push_str(&by_tier[tier.index()].line(tier.label()));
    }
    report.push_str(&format!("\nby what the person was doing\n{HEADER}"));
    for mode in Mode::ALL {
        report.push_str(&by_mode[mode.index()].line(mode.label()));
    }

    report.push_str(&format!(
        "\noverall:\n  \
         the gate admits a correct snapshot for {} of {asked} questions\n  \
         BM25 alone puts the answer in front of the model for {} of {asked}\n  \
         a PERFECT semantic backend manages {} of {asked} on the tool path\n  \
         ...and {} of {asked} on the speculative path\n  \
         the seam consulted the backend {} times across {} opportunities\n",
        overall.gate.passed,
        overall.lexical_only.passed,
        overall.oracle_interactive.passed,
        overall.oracle_speculative.passed,
        oracle.calls(),
        asked * 2,
    ));

    if !gate_refused.is_empty() {
        report.push_str(&format!(
            "\nquestions the gate refuses even when the snapshot holds the answer ({}):\n",
            gate_refused.len()
        ));
        for question in gate_refused.iter().take(20) {
            report.push_str(&format!("  {question}\n"));
        }
        if gate_refused.len() > 20 {
            report.push_str(&format!("  … and {} more\n", gate_refused.len() - 20));
        }
    }
    eprintln!("{report}");

    // First, that the measurement can fail at all. An oracle nobody consults or
    // a corpus BM25 already answers would make every column agree and the file
    // a ceremony.
    assert!(
        overall.lexical_only.passed < asked,
        "BM25 alone already answers every question, so nothing here can \
         distinguish the conditions — the query set has stopped being hard\n{report}"
    );
    assert_eq!(
        oracle.calls(),
        asked * 2,
        "the seam declined to consult the semantic backend on some queries. \
         There is no longer a gate that should do that: it was measured asking \
         on 13 of 93 questions and rescuing all 13.\n{report}"
    );

    // Then the guarantee the five fixes exist to provide: whatever the backend
    // knows, the model gets. This is a statement about plumbing, not about
    // retrieval quality — the oracle is perfect precisely so that anything less
    // than everything is the engine's fault.
    for (label, tally) in [
        ("the recall_context path", &overall.oracle_interactive),
        ("the speculative path", &overall.oracle_speculative),
    ] {
        assert_eq!(
            tally.passed,
            asked,
            "{label} lost {} of {asked} answers a perfect semantic backend had \
             already found. Something between the backend and the payload is \
             discarding results — a reinstated gate, a ranking that cannot \
             compete, a budget of zero, or the per-predicate cap.\n{report}",
            asked - tally.passed,
        );
    }
}

/// Why a `satisfies` refusal must never again discard the snapshot.
///
/// `satisfies` tests word overlap between the question and the prepared
/// statements. That is the right question for "can I skip the search entirely"
/// and the wrong one for "is this snapshot any good", because the questions a
/// semantic layer exists to answer are exactly the ones carrying no overlap.
///
/// This pins the premise of the fusion in [`gemini_memory_rs::retrieval::fuse_snapshots`]:
/// while `satisfies` still refuses correct snapshots in bulk, discarding one on
/// refusal throws away answers. If the day comes that this test fails —
/// `satisfies` having become semantic itself — the fusion is no longer load
/// bearing and both can be revisited.
#[tokio::test]
async fn the_snapshot_gate_still_refuses_snapshots_that_hold_the_answer() {
    let records = corpus_records().await;
    let active: Vec<&CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();
    let now = Utc::now();

    let mut refused = Vec::new();
    for (probe_name, phrasing) in paraphrase::all() {
        let probe = probe(probe_name);
        let target = corpus::by_id(&records, probe.target).expect("target in corpus");
        let filler: Vec<&CanonicalMemory> = active
            .iter()
            .copied()
            .filter(|m| m.id != target.id)
            .take(4)
            .collect();
        if !snapshot_holding(target, &filler).satisfies(phrasing.query, now) {
            refused.push(phrasing.query);
        }
    }

    assert!(
        !refused.is_empty(),
        "`satisfies` no longer refuses any correct snapshot. That would be good \
         news, and it means the fusion in `fuse_snapshots` has stopped being \
         load bearing — revisit both rather than leaving this test asserting a \
         premise that no longer holds."
    );
    eprintln!(
        "\n`satisfies` refuses {} of {} correct snapshots, e.g.:\n  {}\n",
        refused.len(),
        paraphrase::count(),
        refused
            .iter()
            .take(5)
            .map(|q| format!("{q:?}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// Which transcript the snapshot in front of the model was actually built from.
///
/// `begin_turn` promotes whatever `prepare` wrote last, so the order of those
/// two calls decides how stale the model's context is. `TurnExtractor` used to
/// run `begin_turn(next)` and *then* `prepare(next, …)`, which published the
/// previous round's speculation and left the fresh one sitting unread in
/// `prepared` for a whole turn: the snapshot serving turn N was built from the
/// transcript of turn N−2.
///
/// This is not a style point. It is the difference between a user saying "I'm
/// meeting Rhea for dinner" and being understood on the next sentence, or on
/// the one after that.
///
/// This test pins the corrected order by asserting the *consequence* rather
/// than the call sequence, so it fails if anyone reintroduces the delay by any
/// route.
#[tokio::test]
async fn the_snapshot_serving_a_turn_was_built_from_the_previous_utterance() {
    let scratch = ScratchDir::new("speculation-staleness");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let session = engine.begin_session(SessionId::new("ses_staleness"));

    // Turn 1 is about the bicycle; turn 2 is about the barber.
    let bicycle = probe("possession");
    let barber = probe("corrected_barber");

    // The production call order: speculate on what was just said, then open
    // the turn that will read it.
    session.prepare(TurnId(1), bicycle.ask).await.unwrap();
    session.begin_turn(TurnId(1));

    session.prepare(TurnId(2), barber.ask).await.unwrap();
    session.begin_turn(TurnId(2));

    let statements = |s: &PreparedMemorySnapshot| {
        s.facts
            .iter()
            .map(|f| f.statement.clone())
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let active_text = statements(&session.active_snapshot());

    eprintln!("\nturn 2 is in flight. serving the model: {active_text}\n");

    assert!(
        says_any(&active_text, barber.expect).is_some(),
        "turn 2 is being served context built from turn 1's utterance or \
         earlier. The speculation from the utterance immediately before it — \
         about the barber — never reached the model.\n  serving: {active_text}"
    );
    assert!(
        says_any(&active_text, bicycle.expect).is_none(),
        "turn 2 is still holding turn 1's bicycle context, so the promotion is \
         not replacing the snapshot.\n  serving: {active_text}"
    );
}

/// What the model is handed end to end when speculation is on-topic, with **no
/// semantic backend at all**.
///
/// The warm case is the friendliest one the product ever sees: the user
/// mentioned the subject in the previous breath. It is also where the single
/// largest win in this change shows up, and it costs nothing — no embeddings,
/// no model, no second index.
///
/// It measured 59 of 93 while a `satisfies` refusal discarded the prepared
/// snapshot and fell back to a lexical search, and 81 of 93 once a refusal
/// demotes the snapshot to one ranking of two instead. Twenty-two questions,
/// bought purely by not throwing away work the engine had already done. That is
/// the argument that the gate was the bug rather than the ranking.
#[tokio::test]
async fn end_to_end_recall_when_the_speculation_was_on_topic() {
    let scratch = ScratchDir::new("speculation-warm");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let session = engine.begin_session(SessionId::new("ses_warm"));

    let mut answered = 0usize;
    let mut asked = 0usize;
    let mut missed: Vec<String> = Vec::new();

    for (probe_name, phrasing) in paraphrase::all() {
        let probe = probe(probe_name);
        // Warm: speculate on the topic, twice, so the off-by-one above cannot
        // be what causes a miss. This measures the gate and the ranking, not
        // the pipeline's staleness.
        session.begin_turn(TurnId(1));
        session.prepare(TurnId(1), probe.ask).await.unwrap();
        session.begin_turn(TurnId(2));
        session.prepare(TurnId(2), probe.ask).await.unwrap();

        let payload = session.recall(phrasing.query, TurnId(2)).await;
        let facts = payload_statements(&payload);
        asked += 1;
        if facts.iter().any(|f| says_any(f, probe.expect).is_some()) {
            answered += 1;
        } else {
            missed.push(format!(
                "[{}/{}] {:?}",
                phrasing.tier.label(),
                phrasing.mode.label(),
                phrasing.query
            ));
        }
    }

    eprintln!(
        "\nwarm speculation, end to end: {answered}/{asked} questions had the answer \
         in the payload.\n{} missed.\n",
        missed.len()
    );
    // A floor rather than the measured 81, because this runs through the whole
    // engine and a few questions sit near the per-predicate cap. But it is well
    // above the 58 that BM25 alone manages and the 59 this measured while a
    // refusal discarded the snapshot, so a regression to either shows up here.
    assert!(
        answered >= 70,
        "on-topic speculation put the answer in front of the model for only \
         {answered} of {asked} questions. It measured 81 once a `satisfies` \
         refusal stopped discarding the prepared snapshot; 58 is what lexical \
         search manages with no speculation at all, so a number near that means \
         the snapshot is being thrown away again.\nmissed:\n  {}",
        missed.join("\n  ")
    );
}
