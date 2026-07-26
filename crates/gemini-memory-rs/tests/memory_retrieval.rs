//! Retrieval, examined rather than sampled.
//!
//! [`memory_at_scale`](../memory_at_scale/index.html) asks whether retrieval
//! still works as the corpus grows. This file asks the narrower questions that
//! decide whether an answer is safe to say out loud:
//!
//! - does the right record come back *first*, against a corpus full of records
//!   that look like answers to the same question;
//! - does a question memory cannot answer come back empty, rather than with
//!   five arbitrary facts about the person;
//! - does a fact the user corrected stay gone;
//! - does narrowing a search hide the answer.
//!
//! Nothing here reaches the network, so it runs on every `cargo test` and a
//! ranking regression fails in CI rather than waiting for someone with
//! credentials to notice.

mod common;

use common::corpus::{self, payload_statements, says, says_any, PROBES, UNANSWERABLE};
use common::{file_backed_engine, ScratchDir};

use gemini_memory_rs::core::{MemoryStatus, SessionId, TurnId};
use gemini_memory_rs::engine::{MemoryEngine, MemorySession};
use gemini_memory_rs::runtime::RecallScope;

async fn seeded(label: &str) -> (ScratchDir, MemoryEngine) {
    let scratch = ScratchDir::new(label);
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    (scratch, engine)
}

fn session(engine: &MemoryEngine) -> MemorySession {
    let session = engine.begin_session(SessionId::new("ses_probe"));
    session.begin_turn(TurnId(1));
    session
}

// ─── the fixture's own preconditions ────────────────────────────────────────

/// Every assertion in this file is worthless if the corpus is small, if a
/// needle's answer token appears twice, or if a "trap" is not actually
/// competing. A fixture that quietly stops being hard is the most expensive
/// kind of green test, so its properties are asserted rather than assumed.
#[tokio::test]
async fn the_corpus_is_large_and_every_needle_is_unique() {
    let (_scratch, engine) = seeded("corpus-shape").await;
    let records = corpus::installed(&engine).await;
    let active: Vec<_> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();

    assert!(
        active.len() >= 1_000,
        "the corpus is not big enough to be wrong about: {} active records",
        active.len()
    );

    for probe in PROBES {
        let target = corpus::by_id(&records, probe.target)
            .unwrap_or_else(|| panic!("{}: target {} is missing", probe.name, probe.target));

        // The answer token occurs exactly once across every statement, so a
        // correct answer cannot have come from anywhere else — which is what
        // makes the live assertion a single word.
        for token in probe.expect {
            let holders: Vec<&str> = records
                .iter()
                .filter(|m| says(&m.statement, token))
                .map(|m| m.id.as_str())
                .collect();
            assert_eq!(
                holders,
                vec![probe.target],
                "{}: `{token}` should identify exactly the target record",
                probe.name
            );
        }

        // Each forbidden token belongs to a record that exists and competes,
        // rather than to an imaginary wrong answer.
        for token in probe.forbid {
            assert!(
                !says(&target.statement, token),
                "{}: `{token}` is forbidden but appears in the target itself",
                probe.name
            );
            assert!(
                records.iter().any(|m| says(&m.statement, token)),
                "{}: `{token}` is forbidden but nothing says it, so it proves nothing",
                probe.name
            );
        }

        let rivals = records
            .iter()
            .filter(|m| m.predicate == target.predicate && m.id != target.id)
            .count();
        assert!(
            rivals >= 2,
            "{}: predicate `{}` has {rivals} competing record(s) — too easy",
            probe.name,
            target.predicate.as_str()
        );
    }
}

// ─── the answer comes back, and comes back first ────────────────────────────

/// Ranked first, not merely present.
///
/// `max_memories` is 5, so "in the results" is a much weaker statement than it
/// looks, and the model reads the list top-down.
#[tokio::test]
async fn every_needle_outranks_the_corpus_around_it() {
    let (_scratch, engine) = seeded("corpus-rank").await;
    let session = session(&engine);
    let records = corpus::installed(&engine).await;

    let mut report = String::from("\nprobe                  rank  top result\n");
    let mut failures = Vec::new();

    for probe in PROBES {
        let facts = payload_statements(&session.recall(probe.query, TurnId(1)).await);
        let target = corpus::by_id(&records, probe.target).expect("target in corpus");
        let rank = facts
            .iter()
            .position(|f| says(f, &target.statement))
            .map(|i| i + 1);

        report.push_str(&format!(
            "{:<22} {:<5} {}\n",
            probe.name,
            rank.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
            facts
                .first()
                .map(String::as_str)
                .unwrap_or("(nothing found)")
        ));

        match rank {
            Some(1) => {}
            Some(r) => failures.push(format!(
                "{}: the answer ranked {r} of {}, behind {:?}",
                probe.name,
                facts.len(),
                &facts[..r - 1]
            )),
            None => failures.push(format!(
                "{}: the answer was not retrieved at all; got {facts:?}",
                probe.name
            )),
        }

        // Ranking first is not enough if a trap rode in beside it: a decoy in
        // the top slot is a decoy the model can speak.
        if let Some(leaked) = facts.first().and_then(|top| says_any(top, probe.forbid)) {
            failures.push(format!(
                "{}: the top result carries the forbidden token `{leaked}`: {}",
                probe.name,
                facts.first().unwrap()
            ));
        }
    }

    eprintln!("{report}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A question the corpus cannot answer gets no answer.
///
/// At this size *something* scores above zero for almost any query, and the
/// wrong behaviour is not a miss — it is five unrelated facts about the person
/// handed to a model that was asked to be helpful. Each of these is phrased the
/// way a model writes a memory lookup, in the third person, because that
/// phrasing is what used to defeat the score floor: the corpus's own subject
/// form appears in most records and in the highest-weighted field.
#[tokio::test]
async fn a_question_the_corpus_cannot_answer_returns_nothing() {
    let (_scratch, engine) = seeded("corpus-empty").await;
    let session = session(&engine);

    for query in UNANSWERABLE {
        let payload = session.recall(query, TurnId(1)).await;
        assert_eq!(
            payload["status"],
            "not_found",
            "`{query}` should find nothing, got {:?}",
            payload_statements(&payload)
        );
    }
}

/// A corrected fact's old value is on disk and must stay there.
///
/// The superseded barber sits in `superseded/`, parsed back into the namespace
/// on load like any other record. The only thing keeping it from the model is
/// the status filter in the index build, which this asserts by asking the
/// question that would surface it.
#[tokio::test]
async fn a_superseded_record_never_reaches_the_model() {
    let (_scratch, engine) = seeded("corpus-superseded").await;
    let records = corpus::installed(&engine).await;

    assert!(
        records
            .iter()
            .any(|m| m.id.as_str() == "mem_barber_old" && m.status == MemoryStatus::Superseded),
        "the superseded record was not persisted, so this test proves nothing"
    );

    let session = session(&engine);
    for query in [
        "the user's barber, who cuts their hair",
        "Girish barber salon",
        "Tuloma Salon haircut",
    ] {
        let facts = payload_statements(&session.recall(query, TurnId(1)).await);
        assert!(
            !facts.iter().any(|f| says(f, "girish")),
            "`{query}` surfaced the superseded barber: {facts:?}"
        );
    }
}

/// Narrowing the scope wrongly does not fail safe.
///
/// `scope` is a hard kind filter chosen by the *model*, which cannot see the
/// taxonomy it is choosing from. Get it wrong and the answer is not ranked
/// lower, it is excluded — while lower-relevance records from the slice that
/// was picked come back looking like the best memory has.
///
/// Observed against the live API: asked where they were collecting a family
/// member's cake from, the model searched `persistent`, because a standing
/// arrangement reads like a durable fact. The commitment was filtered out and
/// somebody else's errand came back instead. The `RecallScope` descriptions now
/// spell out what each slice excludes; this test is what says whether that is
/// still needed.
#[tokio::test]
async fn a_wrongly_narrowed_scope_hides_the_answer_rather_than_ranking_it_lower() {
    let (_scratch, engine) = seeded("corpus-scope").await;
    let session = session(&engine);
    let query = "where the user promised to collect Priya's cake";

    let unscoped = payload_statements(&session.recall(query, TurnId(1)).await);
    assert!(
        unscoped.first().is_some_and(|top| says(top, "ashgrove")),
        "an unscoped recall should answer this outright: {unscoped:?}"
    );

    let narrowed = payload_statements(
        &session
            .recall_scoped(query, TurnId(1), RecallScope::Persistent)
            .await,
    );
    assert!(
        !narrowed.iter().any(|s| says(s, "ashgrove")),
        "an errand is not a persistent-scope kind, so this test documents the wrong \
         thing if it comes back: {narrowed:?}"
    );
    assert!(
        !narrowed.is_empty(),
        "the hazard is that something plausible arrives in the answer's place — a \
         narrowed recall that came back empty would at least be honest about \
         knowing nothing, and this test would be obsolete"
    );
}

// ─── a limitation worth naming ──────────────────────────────────────────────

/// `recall_context` exists to recall context *about this user* — its own
/// description says so. But a query naming no entity is searched exactly like
/// one that does, so a bare topic word ranks by lexical match alone and records
/// about other people can win it. Nothing says a memory lookup is about the
/// user unless told.
///
/// Not hypothetical: driving a live session, the model wrote a bare one-word
/// query for a question asked in the first person, because the subject is
/// implied by the tool's purpose. Records about other people came back,
/// `max_per_predicate` (2) was filled by two of them, the user's own record
/// never reached the model, and the model correctly said it did not know.
///
/// The cost is asymmetric. A missing fact is a shrug; the same ranking with a
/// less careful instruction is an assistant confidently naming someone else's
/// barber, allergy or errand as yours.
///
/// Two ways out, both ranking decisions rather than test fixes: make a query
/// that names no subject prefer records whose subject is the user, or give
/// subject agreement weight proportional to the lexical score instead of the
/// flat `ENTITY_BASE` of 2.0 — worth ~6% against scores in the tens, and unable
/// to move a record past a better lexical match.
#[tokio::test]
#[ignore = "known limitation: a subject-less recall does not default to the user"]
async fn a_subject_less_recall_does_not_default_to_the_user() {
    use gemini_memory_rs::core::{EntityRef, MemoryKind};

    let scratch = ScratchDir::new("corpus-subject");
    let engine = file_backed_engine("usr_subject", scratch.path());
    let owner = engine.user().clone();

    let mut records = vec![corpus::make(
        &owner,
        "mem_mine",
        MemoryKind::Preference,
        "barber",
        EntityRef::user(),
        "The user's barber is Deepa at Tuloma Salon.",
        &["barber", "haircut", "salon"],
        &["who cuts my hair", "hairdresser"],
    )];
    for (i, (who, whose)) in [("Rhea", "Anisha"), ("Priya", "Kabini"), ("Devan", "Faraz")]
        .iter()
        .enumerate()
    {
        records.push(corpus::make(
            &owner,
            &format!("mem_theirs_{i}"),
            MemoryKind::RelationshipPreference,
            "barber",
            EntityRef::named(*who),
            &format!("{who}'s hairdresser is {whose}."),
            &["barber", "haircut", "salon", "hairdresser", "hair"],
            &[&format!("who cuts {who}'s hair")],
        ));
    }
    corpus::install_records(&engine, records).await;

    let session = session(&engine);
    let facts = payload_statements(&session.recall("hairdresser", TurnId(1)).await);

    assert!(
        facts.first().is_some_and(|top| says(top, "deepa")),
        "a recall that names nobody answered with someone else's hairdresser rather \
         than the user's own: {facts:?}"
    );
}

/// A correction the user was told had been applied, which was not.
///
/// `manage_memory(correct, …)` returns `{"status":"accepted",
/// "effective_in_session":true}` and the assistant says so out loud. The
/// canonical record it contradicts is then supposed to be hidden for the rest
/// of the conversation — the engine suppresses the whole `subject|predicate`
/// window so a correction cannot be answered with the thing just corrected.
///
/// It is not hidden, because the two do not agree on a predicate. The rule-based
/// command path names the fact from a small topic table and falls back to a bare
/// `preference`, so the window it suppresses is `user|preference` while the
/// record lives at `user|beverage_preference`. Nothing matches, nothing is
/// suppressed, and the next recall returns the old value *and* the new one — two
/// contradicting facts handed to a model that was told both are current.
///
/// Observed end to end against the live API: asked for the coffee order, told
/// "a cortado"; corrected to a doppio; the assistant replied "I've corrected
/// that for you"; asked again, it said "a cortado".
///
/// The engine already has the mechanism that prevents this — `known_predicates`
/// exists so a correction lands on the predicate it is correcting, and the
/// extraction prompt spends a paragraph on it — but the `manage_memory` path
/// never consults it. Resolving the target window from the corpus before
/// suppressing, rather than inventing a predicate from the text, is the fix.
#[tokio::test]
#[ignore = "known defect: an explicit correction does not suppress the record it corrects"]
async fn an_explicit_correction_hides_the_record_it_corrects() {
    use gemini_memory_rs::core::MutationIntent;

    let (_scratch, engine) = seeded("corpus-correction").await;
    let session = session(&engine);

    let before = payload_statements(
        &session
            .recall("the user's usual coffee order", TurnId(1))
            .await,
    );
    assert!(
        before.iter().any(|s| says(s, "cortado")),
        "the corpus did not hold the fact to be corrected: {before:?}"
    );

    let accepted = session
        .apply_explicit_command(
            MutationIntent::Correct,
            "my usual coffee order is a doppio now, not a cortado",
            TurnId(1),
        )
        .await
        .expect("the correction is accepted");
    assert_eq!(accepted["effective_in_session"], true);

    session.begin_turn(TurnId(2));
    let after = payload_statements(
        &session
            .recall("the user's usual coffee order", TurnId(2))
            .await,
    );

    assert!(
        after.iter().any(|s| says(s, "doppio")),
        "the corrected value is not being served: {after:?}"
    );
    assert!(
        !after.iter().any(|s| says(s, "cortado")),
        "the superseded value is still served alongside its own correction, one turn \
         after the user was told the correction had been applied: {after:?}"
    );
}
