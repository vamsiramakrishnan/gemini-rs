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
//! Every needle in the corpus is asked for a dozen times over, along two axes.
//! The first is **difficulty** — how far the question sits from the words of
//! the record that answers it:
//!
//! | Tier | What it is | Example, for "The user's usual coffee order is a cortado." |
//! |---|---|---|
//! | `Echo` | the words a model writes when it echoes the topic | "the user's usual coffee order" |
//! | `Direct` | a person asking plainly | "what coffee do I usually order" |
//! | `Synonym` | the same idea, different vocabulary | "my go-to drink at a cafe" |
//! | `Indirect` | the situation described rather than the attribute named | "I'm at the counter, what do I normally get" |
//! | `Inferential` | needs a step of reasoning to connect | "order me the same as always" |
//!
//! The second is **what the person was doing**, which is the axis a product
//! cares about: a well-formed question, a hands-full command, a thing pointed
//! at, an occasion named instead of a topic, a constraint stated instead of a
//! fact, a comparison, a time window, a referring phrase. Those produce very
//! different query strings, and the difference between them is larger than the
//! difference between tiers. See [`common::paraphrase::Mode`].
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

use common::corpus::{self, payload_statements, says_any, PROBES};
use common::paraphrase::{self, overlap, Mode, Tier};
use common::{file_backed_engine, ScratchDir};

use gemini_memory_rs::core::{SessionId, TurnId};

/// One bucket's totals.
#[derive(Default, Clone, Copy)]
struct Tally {
    asked: usize,
    first: usize,
    top_five: usize,
    empty: usize,
    overlap: f64,
}

impl Tally {
    fn record(&mut self, rank: Option<usize>, empty: bool, overlap: f64) {
        self.asked += 1;
        self.overlap += overlap;
        match rank {
            Some(0) => {
                self.first += 1;
                self.top_five += 1;
            }
            Some(r) if r < 5 => self.top_five += 1,
            _ => {}
        }
        if empty {
            self.empty += 1;
        }
    }

    fn row(&self, label: &str) -> String {
        format!(
            "{:<15} {:<6} {:<11} {:<9} {:<11} {:<14} {:.0}%\n",
            label,
            self.asked,
            format!("{}/{}", self.first, self.asked),
            format!("{}/{}", self.top_five, self.asked),
            format!("{}/{}", self.asked - self.top_five - self.empty, self.asked),
            format!("{}/{}", self.empty, self.asked),
            if self.asked == 0 {
                0.0
            } else {
                100.0 * self.overlap / self.asked as f64
            },
        )
    }
}

/// Run every phrasing, tallied by both axes.
async fn measure() -> ([Tally; Tier::COUNT], [Tally; Mode::COUNT], Vec<String>) {
    let scratch = ScratchDir::new("paraphrase");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let records = corpus::installed(&engine).await;
    let session = engine.begin_session(SessionId::new("ses_paraphrase"));
    session.begin_turn(TurnId(1));

    let mut by_tier: [Tally; Tier::COUNT] = Default::default();
    let mut by_mode: [Tally; Mode::COUNT] = Default::default();
    let mut silent = Vec::new();

    for (probe_name, phrasing) in paraphrase::all() {
        let probe = PROBES
            .iter()
            .find(|p| p.name == probe_name)
            .unwrap_or_else(|| panic!("no probe named {probe_name}"));
        let target = corpus::by_id(&records, probe.target).expect("target in corpus");

        let facts = payload_statements(&session.recall(phrasing.query, TurnId(1)).await);
        let rank = facts
            .iter()
            .position(|f| says_any(f, probe.expect).is_some());
        let empty = facts.is_empty();
        let overlap = overlap(phrasing.query, &target.statement);

        by_tier[phrasing.tier.index()].record(rank, empty, overlap);
        by_mode[phrasing.mode.index()].record(rank, empty, overlap);
        if empty {
            silent.push(format!(
                "[{}/{}] {:?}",
                phrasing.tier.label(),
                phrasing.mode.label(),
                phrasing.query
            ));
        }
    }

    (by_tier, by_mode, silent)
}

/// The measurement, printed as two tables and asserted only where it holds.
///
/// Run it with `cargo test -p gemini-memory-rs --test memory_paraphrase --
/// --nocapture` to see the shape of the loss.
#[tokio::test]
async fn how_far_retrieval_falls_when_the_question_is_not_phrased_like_the_answer() {
    let (by_tier, by_mode, silent) = measure().await;

    let header =
        "kind            asked  answered@1  in top 5  wrong only  found nothing  overlap\n";
    let mut report = format!(
        "\nrecall over {} questions, 1,200-record corpus, lexical only\n\n\
         by how far the question sits from the record's own words\n{header}",
        paraphrase::count()
    );
    for tier in Tier::ALL {
        report.push_str(&by_tier[tier.index()].row(tier.label()));
    }
    report.push_str(&format!(
        "\nby what the person was doing when they said it\n{header}"
    ));
    for mode in Mode::ALL {
        report.push_str(&by_mode[mode.index()].row(mode.label()));
    }

    let asked: usize = by_tier.iter().map(|t| t.asked).sum();
    let first: usize = by_tier.iter().map(|t| t.first).sum();
    let empty: usize = by_tier.iter().map(|t| t.empty).sum();
    let top_five: usize = by_tier.iter().map(|t| t.top_five).sum();
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
        "\nRecall tracks the overlap column, not the tier label, because overlap is the only\n\
         signal a lexical index has. The mode table is the same fact told the way a product\n\
         manager needs it: which kinds of moment this retriever is useless in.\n",
    );
    eprintln!("{report}");

    // Only what holds today, so this is a regression guard rather than a wish.
    let echo = &by_tier[Tier::Echo.index()];
    assert_eq!(
        echo.first, echo.asked,
        "the echo tier is what every other retrieval test in this crate measures; \
         if it regresses, those tests are lying too\n{report}"
    );
    let direct = &by_tier[Tier::Direct.index()];
    assert!(
        direct.top_five * 2 >= direct.asked,
        "fewer than half of plainly-asked questions found their answer anywhere in \
         the results — below what the engine managed when this was written\n{report}"
    );
}

/// The target, once there is a semantic layer.
///
/// Not a wish-list: this is the threshold at which the failures above stop
/// being product failures. Somebody asking "who's my hairdresser" or "is tahini
/// safe for me" is asking an ordinary question, and a memory that answers "I
/// don't know" is a memory that does not work, however good its BM25 is.
///
/// Deliberately not perfection. Inferential questions ask retrieval to know
/// that a bagel might have sesame on it, which is reasoning rather than
/// retrieval; three quarters overall, with nothing worse than half in any
/// single tier or mode, is the bar for calling the semantic layer done.
#[tokio::test]
#[ignore = "needs the SemanticFallback seam implemented — this is its acceptance test"]
async fn paraphrase_recall_survives_the_way_people_actually_talk() {
    let (by_tier, by_mode, _) = measure().await;
    let asked: usize = by_tier.iter().map(|t| t.asked).sum();
    let first: usize = by_tier.iter().map(|t| t.first).sum();

    for tier in Tier::ALL {
        let t = &by_tier[tier.index()];
        assert!(
            t.first * 2 >= t.asked,
            "{}: only {}/{} answered by the top result",
            tier.label(),
            t.first,
            t.asked
        );
    }
    for mode in Mode::ALL {
        let m = &by_mode[mode.index()];
        assert!(
            m.first * 2 >= m.asked,
            "{}: only {}/{} answered by the top result",
            mode.label(),
            m.first,
            m.asked
        );
    }
    assert!(
        first * 4 >= asked * 3,
        "overall {first}/{asked} answered by the top result, target is three quarters"
    );
}
