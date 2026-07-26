//! How much retrieval quality is lost for want of a semantic layer, measured.
//!
//! # The question
//!
//! Every other retrieval test in this crate asks its questions in words the
//! corpus already uses. That is not dishonest — a model writing a
//! `recall_context` query often does echo the topic — but it measures the
//! easiest case, and it is the case a lexical index is *guaranteed* to win.
//! People do not talk that way. They say "my go-to drink", not "the user's
//! usual coffee order"; "who's my hairdresser", not "the user's barber"; "is
//! tahini safe for me", which shares no word at all with "The user is allergic
//! to sesame."
//!
//! BM25 cannot bridge any of that. `SemanticFallback` is the seam meant to, and
//! it has no implementation. This file measures the size of the hole.
//!
//! # The method
//!
//! Every needle in the corpus is asked for five times, in five registers a
//! person might use:
//!
//! | Tier | What it is | Example, for "The user's usual coffee order is a cortado." |
//! |---|---|---|
//! | `Echo` | the words a model writes when it echoes the topic | "the user's usual coffee order" |
//! | `Direct` | a person asking plainly | "what coffee do I usually order" |
//! | `Synonym` | the same idea, different vocabulary | "my go-to drink at a cafe" |
//! | `Indirect` | the situation described rather than the attribute named | "I'm at the counter, what do I normally get" |
//! | `Inferential` | needs a step of reasoning to connect | "order me the same as always" |
//!
//! # Why the tiers are not just my opinion
//!
//! The report prints, for each tier, the **measured content-word overlap**
//! between the question and the record that answers it — the objective quantity
//! the labels are only a proxy for. And the labels turn out to be the weaker
//! predictor: `synonym` scores worse than `inferential`, because swapping every
//! content word strips more signal than adding a reasoning step does. Sorted by
//! the overlap column instead, recall is monotonic. That is the finding rather
//! than an inconvenience: a lexical index has exactly one signal, and its
//! quality is a function of how much of that signal the question happens to
//! carry.
//!
//! # What this file asserts
//!
//! Only the tiers that work today, as a regression guard. The rest is reported,
//! not asserted — a test that demanded the current engine answer an inferential
//! question would fail forever and tell nobody anything. The target for a
//! semantic layer is written down in
//! [`paraphrase_recall_survives_the_way_people_actually_talk`], which is
//! `#[ignore]`d and will pass when there is one.

mod common;

use std::collections::HashSet;

use common::corpus::{self, payload_statements, says_any, PROBES};
use common::{file_backed_engine, ScratchDir};

use gemini_memory_rs::bm25::tokenize;
use gemini_memory_rs::core::{SessionId, TurnId};

/// How far a question sits from the wording of the record that answers it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tier {
    /// The words a model writes when it echoes the topic back.
    Echo,
    /// A person asking plainly, in the first person.
    Direct,
    /// The same idea in different vocabulary.
    Synonym,
    /// The situation described rather than the attribute named.
    Indirect,
    /// Needs a step of reasoning to connect question to fact.
    Inferential,
}

impl Tier {
    const ALL: [Tier; 5] = [
        Tier::Echo,
        Tier::Direct,
        Tier::Synonym,
        Tier::Indirect,
        Tier::Inferential,
    ];

    fn label(self) -> &'static str {
        match self {
            Tier::Echo => "echo",
            Tier::Direct => "direct",
            Tier::Synonym => "synonym",
            Tier::Indirect => "indirect",
            Tier::Inferential => "inferential",
        }
    }
}

/// The five ways each needle is asked for.
///
/// Ordered to match [`Tier::ALL`]. Written from the *question* side — what
/// somebody talking to a pair of glasses would actually say — rather than by
/// mutating the record, which is the only way to keep the harder tiers honest.
struct Phrasings {
    /// The needle these ask about, by probe name.
    probe: &'static str,
    /// One phrasing per tier, in [`Tier::ALL`] order.
    queries: [&'static str; 5],
}

const PHRASINGS: &[Phrasings] = &[
    Phrasings {
        probe: "coffee_order",
        queries: [
            "the user's usual coffee order",
            "what coffee do I usually order",
            "my go-to drink at a cafe",
            "I'm at the counter, what do I normally get",
            "order me the same as always",
        ],
    },
    Phrasings {
        probe: "allergy",
        queries: [
            "the user's food allergy",
            "what food am I allergic to",
            "which ingredient do I have to avoid",
            "is a tahini dressing safe for me",
            "can I eat this bagel with seeds on top",
        ],
    },
    Phrasings {
        probe: "spouse_restaurant",
        queries: [
            "Rhea's favourite restaurant",
            "where does Rhea like to eat",
            "which place does my wife enjoy dining at",
            "book somewhere my wife will be happy with",
            "she chose the venue last time, which was it",
        ],
    },
    Phrasings {
        probe: "gift_idea",
        queries: [
            "what Rhea wants for her birthday, gift idea",
            "what should I get Rhea for her birthday",
            "any present ideas for my wife",
            "her birthday is coming up and I'm stuck",
            "she keeps dropping hints, about what",
        ],
    },
    Phrasings {
        probe: "climbing_gym",
        queries: [
            "the user's climbing gym",
            "which gym do I climb at",
            "where do I train on the wall",
            "I want to get a session in this evening, where do I go",
            "remind me where my membership is",
        ],
    },
    Phrasings {
        probe: "corrected_barber",
        queries: [
            "the user's barber",
            "who cuts my hair",
            "who is my hairdresser",
            "I need a trim, who do I book with",
            "who do I go to for a fade",
        ],
    },
    Phrasings {
        probe: "errand",
        queries: [
            "where the user promised to collect Priya's cake",
            "where am I picking up Priya's cake",
            "which shop has my sister's order waiting",
            "what do I have to do for Priya on Saturday",
            "I have an errand this weekend, remind me",
        ],
    },
    Phrasings {
        probe: "possession",
        queries: [
            "the user's bicycle",
            "what bike do I ride",
            "what make is my cycle",
            "I'm booking a service, which model is mine",
            "what am I riding to work on",
        ],
    },
];

/// What fraction of a question's content words appear in the record that
/// answers it.
///
/// The objective measure behind the tiers. Uses the index's own tokenizer, so
/// it counts words the way retrieval counts them — same stemming, same stop
/// list — rather than the way they look.
fn overlap(query: &str, statement: &str) -> f64 {
    let asked: HashSet<String> = tokenize(query).into_iter().collect();
    if asked.is_empty() {
        return 0.0;
    }
    let held: HashSet<String> = tokenize(statement).into_iter().collect();
    asked.iter().filter(|t| held.contains(*t)).count() as f64 / asked.len() as f64
}

/// One tier's totals.
#[derive(Default)]
struct Tally {
    asked: usize,
    first: usize,
    top_five: usize,
    empty: usize,
    overlap: f64,
}

/// Run every phrasing and return the per-tier tallies plus the questions that
/// came back with nothing.
async fn measure() -> ([Tally; 5], Vec<String>) {
    let scratch = ScratchDir::new("paraphrase");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let records = corpus::installed(&engine).await;
    let session = engine.begin_session(SessionId::new("ses_paraphrase"));
    session.begin_turn(TurnId(1));

    let mut tallies: [Tally; 5] = Default::default();
    let mut silent = Vec::new();

    for phrasings in PHRASINGS {
        let probe = PROBES
            .iter()
            .find(|p| p.name == phrasings.probe)
            .unwrap_or_else(|| panic!("no probe named {}", phrasings.probe));
        let target = corpus::by_id(&records, probe.target).expect("target in corpus");

        for (i, query) in phrasings.queries.iter().enumerate() {
            let facts = payload_statements(&session.recall(query, TurnId(1)).await);
            let rank = facts
                .iter()
                .position(|f| says_any(f, probe.expect).is_some());

            let tally = &mut tallies[i];
            tally.asked += 1;
            tally.overlap += overlap(query, &target.statement);
            match rank {
                Some(0) => {
                    tally.first += 1;
                    tally.top_five += 1;
                }
                Some(_) => tally.top_five += 1,
                None => {}
            }
            if facts.is_empty() {
                tally.empty += 1;
                silent.push(format!("[{}] {query:?}", Tier::ALL[i].label()));
            }
        }
    }

    (tallies, silent)
}

/// The measurement, printed as a table and asserted only where it holds today.
///
/// Run it with `cargo test -p gemini-memory-rs --test memory_paraphrase --
/// --nocapture` to see the shape of the loss.
#[tokio::test]
async fn how_far_retrieval_falls_when_the_question_is_not_phrased_like_the_answer() {
    let (tallies, silent) = measure().await;

    let mut report = String::from(
        "\nrecall by how the question is phrased (1,200-record corpus, lexical only)\n\
         \n\
         tier          asked  answered@1  in top 5  wrong only  found nothing  overlap\n",
    );
    for (i, tier) in Tier::ALL.iter().enumerate() {
        let t = &tallies[i];
        // The worst outcome is not a miss: it is a confident answer drawn from
        // the wrong neighbourhood, which the model has no way to recognise.
        let wrong_only = t.asked - t.top_five - t.empty;
        report.push_str(&format!(
            "{:<13} {:<6} {:<11} {:<9} {:<11} {:<14} {:.0}%\n",
            tier.label(),
            t.asked,
            format!("{}/{}", t.first, t.asked),
            format!("{}/{}", t.top_five, t.asked),
            format!("{}/{}", wrong_only, t.asked),
            format!("{}/{}", t.empty, t.asked),
            100.0 * t.overlap / t.asked as f64,
        ));
    }

    let asked: usize = tallies.iter().map(|t| t.asked).sum();
    let first: usize = tallies.iter().map(|t| t.first).sum();
    let empty: usize = tallies.iter().map(|t| t.empty).sum();
    let top_five: usize = tallies.iter().map(|t| t.top_five).sum();
    report.push_str(&format!(
        "\noverall {first}/{asked} answered by the top result. {empty}/{asked} returned nothing \
         at all — an honest miss.\n\
         {}/{asked} returned facts that did not contain the answer: a confident wrong \
         neighbourhood,\nwhich is the same shape as a right one and cannot be told apart \
         downstream.\n",
        asked - top_five - empty
    ));
    if !silent.is_empty() {
        report.push_str("\nquestions memory could not answer at all:\n");
        for question in &silent {
            report.push_str(&format!("  {question}\n"));
        }
    }
    report.push_str(
        "\nRecall tracks the overlap column, not the tier label: `synonym` scores worse than\n\
         `inferential` because swapping every content word removes more signal than adding a\n\
         reasoning step does, and the overlap figures say so. That is the finding — a lexical\n\
         index has exactly one signal, and these questions do not carry it.\n",
    );
    eprintln!("{report}");

    // Only what holds today, so this is a regression guard rather than a wish.
    // A model echoing the topic back is the case the current engine is built
    // for, and it has to keep working.
    let echo = &tallies[0];
    assert_eq!(
        echo.first, echo.asked,
        "the echo tier is what every other retrieval test in this crate measures; \
         if it regresses, those tests are lying too\n{report}"
    );
    let direct = &tallies[1];
    assert!(
        direct.top_five * 2 >= direct.asked,
        "fewer than half of plainly-asked questions found their answer anywhere in \
         the results — that is below what the engine managed when this was written\n{report}"
    );
}

/// The target, once there is a semantic layer.
///
/// Not a wish-list: this is the threshold at which the failures above stop
/// being product failures. A person asking "who's my hairdresser" or "is tahini
/// safe for me" is asking an ordinary question, and a memory that answers "I
/// don't know" to it is a memory that does not work, however good its BM25 is.
///
/// Deliberately not set to perfection. Inferential questions ask retrieval to
/// know that a bagel might have sesame on it, which is a reasoning step rather
/// than a retrieval one; three quarters overall, with nothing worse than half
/// in any single tier, is the bar for calling the semantic layer done.
#[tokio::test]
#[ignore = "needs the SemanticFallback seam implemented — this is its acceptance test"]
async fn paraphrase_recall_survives_the_way_people_actually_talk() {
    let (tallies, _) = measure().await;
    let asked: usize = tallies.iter().map(|t| t.asked).sum();
    let first: usize = tallies.iter().map(|t| t.first).sum();

    for (i, tier) in Tier::ALL.iter().enumerate() {
        let t = &tallies[i];
        assert!(
            t.first * 2 >= t.asked,
            "{}: only {}/{} answered by the top result",
            tier.label(),
            t.first,
            t.asked
        );
    }
    assert!(
        first * 4 >= asked * 3,
        "overall {first}/{asked} answered by the top result, target is three quarters"
    );
}
