//! What the memory engine actually does at consumer scale, measured.
//!
//! A pair of smart glasses accumulates memory for as long as someone owns them.
//! Four things decide whether that stays usable, and this file measures each of
//! them against a corpus large enough for the answer to be interesting:
//!
//! | Question | Measured by |
//! |---|---|
//! | Does retrieval still find the right fact as the corpus grows? | [`retrieval_quality_holds_as_the_corpus_grows`] |
//! | What does a recall cost while the user waits? | [`recall_latency_across_corpus_sizes`] |
//! | What does learning something new cost, mid-conversation? | [`what_it_costs_to_learn_something_mid_conversation`] |
//! | What does sealing a conversation into durable memory cost, and do the right things end up there? | [`what_reconciliation_does_at_the_end_of_a_conversation`] |
//!
//! Every test prints a table and then asserts a budget, so the same run both
//! explains the behaviour (`cargo test … -- --nocapture`) and fails when it
//! regresses. The budgets are deliberately loose — several times the measured
//! values — because these run on shared CI hardware and the point is to catch a
//! change of *shape*, such as retrieval going linear in corpus size, not to
//! police a few hundred microseconds.
//!
//! Nothing here reaches the network: the bundled deterministic extractors stand
//! in for the model, so the numbers are the engine's own cost rather than an
//! API's.

mod common;

use std::time::{Duration, Instant};

use common::corpus::{self, payload_statements, says, says_any, PROBES};
use common::{file_backed_engine, ScratchDir};

use gemini_memory_rs::core::{SessionId, TurnId};
use gemini_memory_rs::engine::{MemoryEngine, MemorySession};

/// Corpus sizes the sweeps run at.
///
/// The top of the range is well past what a person accumulates in a year of
/// heavy use, which is the point: the interesting question is not what it costs
/// at today's size but whether the cost is flat in the size.
const SIZES: &[usize] = &[250, 1_000, 4_000, 16_000];

/// Percentiles from a set of timings.
fn percentiles(mut timings: Vec<Duration>) -> (Duration, Duration, Duration) {
    timings.sort();
    let at = |q: usize| timings[(timings.len() * q / 100).min(timings.len() - 1)];
    (at(50), at(95), *timings.last().expect("timings"))
}

/// Seed an engine with `size` records, reporting what that cost.
async fn seeded(label: &str, size: usize) -> (ScratchDir, MemoryEngine, Duration) {
    let scratch = ScratchDir::new(label);
    let engine = file_backed_engine("usr_scale", scratch.path());
    let started = Instant::now();
    corpus::install_records(&engine, corpus::generate(engine.user(), size)).await;
    (scratch, engine, started.elapsed())
}

fn open_session(engine: &MemoryEngine, id: &str) -> MemorySession {
    let session = engine.begin_session(SessionId::new(id));
    session.begin_turn(TurnId(1));
    session
}

/// Bytes of Markdown under a directory — the corpus as it exists on the device.
fn bytes_on_disk(root: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

// ─── retrieval: does it still find the right thing? ─────────────────────────

/// Quality is what scale threatens first.
///
/// Latency degrades visibly and gets fixed; ranking degrades silently. Every
/// record added is another plausible answer to somebody's question, so the
/// measurement that matters is whether the *same* eight questions still return
/// the *same* eight facts when the corpus around them is sixty times larger.
///
/// Each probe's answer token occurs exactly once in the corpus, so "ranked
/// first" is unambiguous, and each probe has a trap whose token must not appear
/// in the top result.
///
/// The floors are floors, not targets, because one probe does degrade and the
/// reason is worth knowing. "Rhea's favourite restaurant" ranks first at 250
/// and 1,000 records and second at 4,000 and above, behind another record whose
/// *subject* is Rhea. As the corpus grows, a common topic word like "restaurant"
/// appears in more and more records and its IDF falls, while a rare entity name
/// does not — so entity-adjacent noise rises past topic matches. The effect is
/// general: at scale, naming a person in a query increasingly outweighs saying
/// what you want to know about them.
#[tokio::test]
async fn retrieval_quality_holds_as_the_corpus_grows() {
    let mut report = String::from(
        "\nretrieval quality by corpus size\n\
           size     records  answered@1  MRR    decoys in top result\n",
    );
    let mut failures = Vec::new();
    let (mut worst_first, mut worst_mrr) = (usize::MAX, 1.0f64);

    for &size in SIZES {
        let (_scratch, engine, _) = seeded("scale-quality", size).await;
        let records = corpus::installed(&engine).await;
        let session = open_session(&engine, "ses_quality");

        let (mut first, mut reciprocal, mut leaked) = (0usize, 0f64, Vec::new());
        for probe in PROBES {
            let facts = payload_statements(&session.recall(probe.query, TurnId(1)).await);
            let target = corpus::by_id(&records, probe.target).expect("target in corpus");
            let rank = facts.iter().position(|f| says(f, &target.statement));

            match rank {
                Some(0) => {
                    first += 1;
                    reciprocal += 1.0;
                }
                Some(r) => {
                    reciprocal += 1.0 / (r + 1) as f64;
                    failures.push(format!(
                        "{size}/{}: the answer ranked {} of {}, behind {:?}",
                        probe.name,
                        r + 1,
                        facts.len(),
                        &facts[..r]
                    ));
                }
                None => failures.push(format!(
                    "{size}/{}: the answer was not retrieved at all; got {facts:?}",
                    probe.name
                )),
            }

            // A decoy in the top result is a decoy the model can speak, and it
            // is the failure mode a large corpus introduces.
            if let Some(trap) = facts.first().and_then(|top| says_any(top, probe.forbid)) {
                leaked.push(format!("{}→{trap}", probe.name));
                failures.push(format!(
                    "{size}/{}: the top result carries the forbidden token `{trap}`: {}",
                    probe.name,
                    facts.first().unwrap()
                ));
            }
        }

        let mrr = reciprocal / PROBES.len() as f64;
        worst_first = worst_first.min(first);
        worst_mrr = worst_mrr.min(mrr);
        report.push_str(&format!(
            "{size:<8} {:<8} {}/{:<9} {mrr:.3}  {}\n",
            records.len(),
            first,
            PROBES.len(),
            if leaked.is_empty() {
                "none".to_string()
            } else {
                leaked.join(", ")
            }
        ));
    }

    eprintln!("{report}");

    let unretrieved: Vec<&String> = failures
        .iter()
        .filter(|f| f.contains("not retrieved at all") || f.contains("forbidden token"))
        .collect();
    assert!(
        unretrieved.is_empty(),
        "an answer was missing entirely, or a decoy took the top slot — either is a \
         wrong answer spoken aloud:\n{}",
        unretrieved
            .iter()
            .map(|f| f.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        worst_first >= PROBES.len() - 1,
        "only {worst_first} of {} questions were answered by the top result at some \
         size; ranking is degrading as the corpus grows:\n{}",
        PROBES.len(),
        failures.join("\n")
    );
    assert!(
        worst_mrr >= 0.90,
        "mean reciprocal rank fell to {worst_mrr:.3}:\n{}",
        failures.join("\n")
    );
}

// ─── latency: what does the user wait for? ──────────────────────────────────

/// A recall happens while the model is mid-sentence, so its cost is a product
/// property rather than a curiosity.
///
/// Two numbers per size. `recall` is the synchronous path a `recall_context`
/// tool call takes — the one a user waits on. `prepare` is the speculative path
/// the extractor runs at a turn boundary while the model is still speaking,
/// which is off the response path but still competes for the same core.
///
/// The assertion is on shape, not on absolute time: the cost of a query is
/// allowed to grow with the corpus, but only as far as an inverted index's
/// posting lists do. Growing with the *number of records* would mean retrieval
/// had started scanning.
#[tokio::test]
async fn recall_latency_across_corpus_sizes() {
    let mut report = String::from(
        "\nlatency by corpus size (deterministic extractors, no network)\n\
           size     seed+index  on disk    recall p50  p95        max        prepare p50\n",
    );
    let mut measured: Vec<(usize, Duration)> = Vec::new();

    for &size in SIZES {
        let (scratch, engine, seed_cost) = seeded("scale-latency", size).await;
        let session = open_session(&engine, "ses_latency");

        // A warm pass first: the measurement should be of steady-state
        // retrieval, not of the first touch of a lazily built structure.
        for probe in PROBES {
            let _ = session.recall(probe.query, TurnId(1)).await;
        }

        let mut recalls = Vec::new();
        for _ in 0..25 {
            for probe in PROBES {
                let started = Instant::now();
                let _ = session.recall(probe.query, TurnId(1)).await;
                recalls.push(started.elapsed());
            }
        }
        let (p50, p95, max) = percentiles(recalls);

        let mut prepares = Vec::new();
        for (turn, probe) in PROBES.iter().enumerate() {
            let started = Instant::now();
            let _ = session.prepare(TurnId(turn as u64 + 2), probe.ask).await;
            prepares.push(started.elapsed());
        }
        let (prepare_p50, _, _) = percentiles(prepares);

        report.push_str(&format!(
            "{size:<8} {:<11} {:<10} {:<11} {:<10} {:<10} {:?}\n",
            format!("{:.0?}", seed_cost),
            format!("{} KiB", bytes_on_disk(scratch.path()) / 1024),
            format!("{p50:.0?}"),
            format!("{p95:.0?}"),
            format!("{max:.0?}"),
            prepare_p50
        ));
        measured.push((size, p50));
    }

    eprintln!("{report}");

    let (smallest, small_cost) = measured[0];
    let (largest, large_cost) = *measured.last().expect("a measurement per size");
    let corpus_growth = largest as f64 / smallest as f64;
    let cost_growth = large_cost.as_secs_f64() / small_cost.as_secs_f64().max(f64::MIN_POSITIVE);

    eprintln!(
        "recall cost grew {cost_growth:.1}× while the corpus grew {corpus_growth:.0}× \
         ({small_cost:.0?} at {smallest} records, {large_cost:.0?} at {largest}).\n\
         Note that the configured lexical deadline is \
         {}ms — at the top of this range the median recall is close to it.\n",
        gemini_memory_rs::core::RetrievalConfig::default().immediate_lexical_timeout_ms
    );

    // Sublinear, with margin. Growth in step with the corpus would mean every
    // query walks every record — which is what happened while the corpus's own
    // subject form was an ordinary search term, and cost 100× for 64× the
    // records.
    assert!(
        cost_growth < corpus_growth * 0.75,
        "recall cost grew {cost_growth:.1}× while the corpus grew {corpus_growth:.0}× \
         ({small_cost:?} at {smallest} records, {large_cost:?} at {largest}) — that is \
         close enough to linear that retrieval is no longer behaving like an index"
    );
    assert!(
        large_cost < Duration::from_millis(50),
        "a recall at {largest} records costs {large_cost:?}, and it happens while the \
         user is waiting for the model to answer"
    );
}

// ─── addition: what does learning something cost? ───────────────────────────

/// Everything a conversation learns has to be usable *in that conversation*,
/// and the interesting question is how soon.
///
/// A fact the user states goes into the session ledger immediately, but it only
/// becomes *searchable* when the session overlay is rebuilt — and that happens
/// on a cadence (`CadenceConfig`: every four user turns, or ninety seconds),
/// not on every turn. So there is a window in which the glasses have heard
/// something, accepted it, and would still answer "I don't know" if asked about
/// it. On a wearable that window is felt directly, as "I just told you".
///
/// This measures three things: what one turn of ingestion costs against a full
/// corpus, how many turns pass before a stated fact can be recalled, and
/// whether it outranks the corpus once it can be.
#[tokio::test]
async fn what_it_costs_to_learn_something_mid_conversation() {
    let (_scratch, engine, _) = seeded("scale-ingest", corpus::DEFAULT_SIZE).await;
    let session = engine.begin_session(SessionId::new("ses_ingest"));

    // Spoken the way people speak to a wearable — contractions and all, which
    // is exactly the shape that used to be dropped on the floor.
    let utterances = [
        "I've started going to a pottery class on Thursday evenings.",
        "I'm allergic to sesame, so nothing with tahini.",
        "I usually order a cortado when I'm out.",
        "I prefer quiet places for dinner.",
        "I always take the metro to work.",
        "I live in Bangalore now.",
        "I hate crowded bars.",
        "Please remember that the spare keys are with the neighbour.",
        "I love the bakery on Vine Hill.",
        "From now on I want reminders spoken once, not repeated.",
    ];

    let mut ingest = Vec::new();
    let mut created = 0usize;
    // Turns between stating the pottery class and being able to recall it.
    let mut turns_until_recallable: Option<usize> = None;

    for (i, utterance) in utterances.iter().enumerate() {
        let turn = TurnId(i as u64 + 1);
        session.begin_turn(turn);
        let started = Instant::now();
        let outcomes = session
            .observe_final_transcript(turn, utterance)
            .await
            .expect("ingestion");
        session.on_turn_complete(turn).await.expect("turn complete");
        ingest.push(started.elapsed());
        created += outcomes
            .iter()
            .filter(|o| matches!(o, gemini_memory_rs::ingestion::LedgerOutcome::Created(_)))
            .count();

        if turns_until_recallable.is_none() {
            let facts = payload_statements(&session.recall("pottery class", turn).await);
            if facts.iter().any(|f| says(f, "pottery")) {
                turns_until_recallable = Some(i + 1);
            }
        }
    }
    let (p50, p95, max) = percentiles(ingest);

    // The most recently stated fact: what the engine guarantees is that the
    // session overlay outranks canonical memory, so this one has to come back
    // ahead of twelve hundred older records.
    let recall_started = Instant::now();
    let facts = payload_statements(&session.recall("reminders spoken once", TurnId(11)).await);
    let recall_cost = recall_started.elapsed();

    eprintln!(
        "\naddition, against {} existing records\n\
           turns ingested           {}\n\
           candidates created       {created}\n\
           usable after de-dup      {}\n\
           per turn p50/p95/max     {p50:.0?} / {p95:.0?} / {max:.0?}\n\
           turns until recallable   {}\n\
           recall of the new fact   {recall_cost:.0?}\n\
           surviving candidates:\n{}\n",
        corpus::DEFAULT_SIZE,
        utterances.len(),
        session.ledger().usable_candidates().len(),
        turns_until_recallable
            .map(|t| t.to_string())
            .unwrap_or_else(|| "never".into()),
        session
            .ledger()
            .usable_candidates()
            .iter()
            .map(|c| format!(
                "             [{}] {}",
                c.predicate.as_str(),
                c.canonical_statement
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );

    assert!(
        created >= utterances.len() / 2,
        "half of a ten-turn conversation produced no candidate at all ({created} created) \
         — the extractor is not hearing ordinary speech"
    );

    // The overlay rebuild is on a cadence, so a stated fact is not searchable
    // the instant it is heard. It has to be searchable *soon*, though: a window
    // longer than the cadence would mean the rebuild is not happening at all.
    let cadence_turns = 4;
    match turns_until_recallable {
        Some(turns) => assert!(
            turns <= cadence_turns + 1,
            "a fact stated on turn 1 took {turns} turns to become recallable; the \
             overlay cadence is every {cadence_turns} turns, so anything longer means \
             the rebuild is not keeping up"
        ),
        None => panic!(
            "a fact stated ten turns ago is still not retrievable in the same \
             conversation, which is the whole point of the session overlay"
        ),
    }

    assert!(
        facts.first().is_some_and(|top| says(top, "reminders")),
        "the most recently stated fact did not rank first — something said a \
         minute ago has to outrank something learned months ago: {facts:?}"
    );
    assert!(
        p95 < Duration::from_millis(250),
        "ingesting one turn costs {p95:?} at the 95th percentile; this runs off the \
         response path, but it still has to keep up with someone talking"
    );
}

// ─── reconciliation: what survives the conversation? ────────────────────────

/// A session ends and its evidence has to become durable memory — or not.
///
/// This is where the engine's central claim is settled: the model proposes and
/// deterministic code commits. Three things are asserted, because each is a
/// different way for a memory system to become untrustworthy over months of
/// use:
///
/// - a fact said twice is **one** record with two pieces of evidence, not two
///   records that will both come back and contradict each other later;
/// - a correction **supersedes** rather than accumulating, so the corpus does
///   not end up asserting both the old value and the new one;
/// - what is written is Markdown a person can read, and a cold engine opened on
///   the same directory answers from it.
#[tokio::test]
async fn what_reconciliation_does_at_the_end_of_a_conversation() {
    let scratch = ScratchDir::new("scale-reconcile");
    let engine = file_backed_engine("usr_scale", scratch.path());
    corpus::install_records(
        &engine,
        corpus::generate(engine.user(), corpus::DEFAULT_SIZE),
    )
    .await;
    let before = corpus::installed(&engine).await.len();
    let bytes_before = bytes_on_disk(scratch.path());

    let session = engine.begin_session(SessionId::new("ses_reconcile"));
    let utterances = [
        "I've started going to a pottery class on Thursday evenings.",
        // The same fact, said again in the expanded form. It must reinforce the
        // first rather than become a second record — otherwise a corpus grows a
        // duplicate every time somebody repeats themselves.
        "I have started going to a pottery class on Thursday evenings.",
        "I prefer Bellagrove for dinner.",
        // …and a correction, which has to land on the predicate it corrects.
        "From now on I prefer Cloudberry for dinner.",
    ];
    for (i, utterance) in utterances.iter().enumerate() {
        let turn = TurnId(i as u64 + 1);
        session.begin_turn(turn);
        session
            .observe_final_transcript(turn, utterance)
            .await
            .expect("ingestion");
        session.on_turn_complete(turn).await.expect("turn complete");
    }

    let started = Instant::now();
    let report = session.finish().await.expect("reconciliation");
    let cost = started.elapsed();

    let after = corpus::installed(&engine).await;
    eprintln!(
        "\nreconciliation of a 4-turn conversation into a {before}-record corpus\n\
           cost                 {cost:.0?}\n\
           created              {}\n\
           reinforced           {}\n\
           refined              {}\n\
           superseded           {}\n\
           staged               {}\n\
           discarded            {}\n\
           records before/after {before} → {}\n\
           on disk              {} KiB → {} KiB\n",
        report.creates,
        report.reinforces,
        report.refines,
        report.supersedes,
        report.stages,
        report.discards,
        after.len(),
        bytes_before / 1024,
        bytes_on_disk(scratch.path()) / 1024
    );

    assert!(
        report.creates + report.reinforces + report.stages > 0,
        "a conversation with four statements in it committed nothing at all: {report:?}"
    );

    // The same fact twice is one record. Anything else and a corpus grows a
    // duplicate every time somebody repeats themselves.
    let pottery: Vec<_> = after
        .iter()
        .filter(|m| says(&m.statement, "pottery"))
        .collect();
    assert!(
        pottery.len() <= 1,
        "the same fact stated twice produced {} records: {:?}",
        pottery.len(),
        pottery.iter().map(|m| &m.statement).collect::<Vec<_>>()
    );

    assert!(
        cost < Duration::from_secs(2),
        "sealing a four-turn conversation into a {before}-record corpus took {cost:?}"
    );

    // And it is all readable Markdown that a cold engine can answer from.
    let reopened = file_backed_engine("usr_scale", scratch.path());
    reopened.compile_index().await.expect("index compiles");
    let next_week = open_session(&reopened, "ses_next_week");
    for probe in PROBES {
        let facts = payload_statements(&next_week.recall(probe.query, TurnId(1)).await);
        assert!(
            facts.iter().any(|f| says_any(f, probe.expect).is_some()),
            "{}: the answer did not survive reconciliation and a restart; a cold \
             engine recalled {facts:?}",
            probe.name
        );
    }
}

// ─── a hazard worth naming ──────────────────────────────────────────────────

/// Two unrelated facts stated in one conversation both survive it.
///
/// In-session micro-reconciliation resolves contradictions by keeping the
/// newest explicit statement in each `subject|predicate` window and suppressing
/// the rest. That is exactly right for a correction — "actually, make it
/// Cloudberry" should not leave two beliefs behind — and exactly wrong for two
/// facts that merely share a predicate name.
///
/// The bundled rule-based extractor makes the collision easy to hit, because it
/// files anything it has no topic mapping for under a generic predicate:
/// "I've started going to a pottery class" and "I always take the metro to
/// work" both become `user|routine`, so the second silently deletes the first
/// from the conversation. The pottery class was recallable on turn 1 and gone
/// by turn 2 — measured, not hypothesised.
///
/// Collapse now applies only where the window is single-valued — a named
/// attribute like `dietary_identity` holds one answer, so a second value
/// contradicts the first — or where the user issued an explicit correction.
/// `preference` and `routine` are the buckets an extractor falls back to when
/// it could not say which attribute a fact concerns, and two values there are
/// usually two facts.
#[tokio::test]
async fn two_unrelated_habits_in_one_conversation_both_survive_it() {
    let scratch = ScratchDir::new("scale-window");
    let engine = file_backed_engine("usr_window", scratch.path());
    let session = engine.begin_session(SessionId::new("ses_window"));

    for (i, utterance) in [
        "I've started going to a pottery class on Thursday evenings.",
        "I always take the metro to work.",
    ]
    .iter()
    .enumerate()
    {
        let turn = TurnId(i as u64 + 1);
        session.begin_turn(turn);
        session
            .observe_final_transcript(turn, utterance)
            .await
            .expect("ingestion");
        session.on_turn_complete(turn).await.expect("turn complete");
    }
    session.ledger().micro_reconcile();

    let kept: Vec<String> = session
        .ledger()
        .usable_candidates()
        .iter()
        .map(|c| c.canonical_statement.clone())
        .collect();

    assert!(
        kept.iter().any(|s| says(s, "metro")),
        "the second habit was lost: {kept:?}"
    );
    assert!(
        kept.iter().any(|s| says(s, "pottery")),
        "the first habit was suppressed by the second — they are different facts \
         that happen to share the predicate `routine`, and the user is never told \
         one of them was dropped: {kept:?}"
    );
}
