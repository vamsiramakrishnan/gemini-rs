//! Whether a semantic layer would ever reach the model, measured before one is
//! built.
//!
//! # Why this exists
//!
//! `semantic_fusion_probe` establishes that embedding the record's own
//! frontmatter answers 66 of 93 paraphrased questions against BM25's 42. That
//! measures an *index*. It says nothing about whether the engine would ever
//! show the model what that index found, and the engine has four separate
//! places where it might not:
//!
//! 1. [`MemoryEngine::begin_session`] constructs `LocalMemoryRetriever::new`
//!    directly, so `with_semantic_fallback` cannot be called on the retriever a
//!    session actually uses.
//! 2. `RetrievalBudget::interactive()` — the budget `recall_context` runs under
//!    — sets `semantic_ms: 0`, so the tool path never consults the backend even
//!    when one is installed.
//! 3. On the speculative path, where it does run, `needs_semantic_fallback`
//!    gates it behind "lexical found too little", and results are appended
//!    *below* every lexical hit as "a safety net, not a competing opinion".
//! 4. The snapshot that speculation produces is then served only if
//!    [`PreparedMemorySnapshot::satisfies`] returns true — a **lexical** overlap
//!    test, on precisely the questions a semantic layer exists to answer.
//!
//! Each was a reasonable decision when written, and the fusion probe's numbers
//! contradict all four. This file measures them rather than arguing about them,
//! because the fix is worth building only if it moves something.
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

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::corpus::{self, payload_statements, says_any, PROBES};
use common::paraphrase::{self, Mode, Tier};
use common::{file_backed_engine, ScratchDir};

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

    // Deliberately not asserted as a target — this is the "before" measurement,
    // and the numbers it prints are the case for the change. What *is* asserted
    // is that the measurement is measuring something: an oracle that is never
    // consulted, or a corpus where every question is already answered, would
    // make the whole file a ceremony.
    assert!(
        overall.lexical_only.passed < asked,
        "BM25 alone already answers every question, so nothing here can \
         distinguish the conditions — the query set has stopped being hard\n{report}"
    );
}

/// The gate, stated as a claim rather than a table.
///
/// A snapshot that literally contains the answer is refused for a large share
/// of the questions a semantic layer exists to answer, because `satisfies`
/// tests **word overlap with the question** — the one signal these questions
/// were built not to carry.
#[tokio::test]
async fn the_snapshot_gate_refuses_snapshots_that_hold_the_answer() {
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
        "expected the lexical gate to refuse some correct snapshots; if this \
         now passes, `satisfies` has been fixed and this test should be \
         inverted into a regression guard"
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
/// `TurnExtractor` runs `begin_turn(next)` and *then* `prepare(next, …)`, and
/// `begin_turn` promotes whatever `prepare` wrote last time. So the snapshot
/// serving turn N was built from the transcript of turn N−2, not N−1: the
/// freshest speculation always sits in `prepared` for a whole turn before
/// anyone can read it.
///
/// This is not a style point. It is the difference between a user saying
/// "I'm meeting Rhea for dinner" and being understood on the next sentence, or
/// on the one after that.
#[tokio::test]
async fn the_active_snapshot_is_two_turns_behind_the_transcript_that_built_it() {
    let scratch = ScratchDir::new("speculation-staleness");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let session = engine.begin_session(SessionId::new("ses_staleness"));

    // Turn 1 is about the bicycle; turn 2 is about the barber. Reproduce the
    // production call order exactly: begin the next turn, then speculate on
    // what was just said.
    let bicycle = probe("possession");
    let barber = probe("corrected_barber");

    session.begin_turn(TurnId(1));
    session.prepare(TurnId(1), bicycle.ask).await.unwrap();

    session.begin_turn(TurnId(2));
    session.prepare(TurnId(2), barber.ask).await.unwrap();

    // Turn 2 is in flight. The user talked about the barber a moment ago and
    // about the bicycle before that.
    let serving_turn_2 = session.active_snapshot();
    let waiting = session.prepared_snapshot();

    let statements = |s: &PreparedMemorySnapshot| {
        s.facts
            .iter()
            .map(|f| f.statement.clone())
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let active_text = statements(&serving_turn_2);
    let waiting_text = statements(&waiting);

    eprintln!(
        "\nturn 2 is in flight.\n  \
         serving the model : {active_text}\n  \
         sat in `prepared` : {waiting_text}\n"
    );

    assert!(
        says_any(&waiting_text, barber.expect).is_some(),
        "the speculation built from the most recent utterance should have found \
         the barber record; it holds: {waiting_text}"
    );
    assert!(
        says_any(&active_text, barber.expect).is_none(),
        "the barber speculation reached the model on the same turn it was \
         built — the off-by-one this test documents has been fixed, and the \
         test should be inverted into a regression guard.\n  \
         active: {active_text}"
    );
}

/// What the model is handed today, end to end, when speculation is on-topic.
///
/// The warm case is the friendliest one the product ever sees: the user
/// mentioned the subject in the previous breath. If the answer does not reach
/// the model here, the speculative path is not carrying the product.
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
    assert!(
        answered > 0,
        "no question was answered end to end even with on-topic speculation — \
         the harness is broken rather than the engine"
    );
}
