//! Whether letting the model emit structured filters is worth building, and in
//! which shape.
//!
//! # The proposal being tested
//!
//! `recall_context` currently takes a `query` string and a three-valued
//! `scope`. The corpus's frontmatter carries far more than that — subject,
//! predicate, kind, mentioned entities, place, temporal scope — and all of it
//! is already indexed. The proposal is to let the model fill in a few of those
//! fields alongside the query, as a small closed DSL, and to use them.
//!
//! The appeal is that it costs nothing in latency. The model is *already*
//! making a tool call; adding arguments to it is free, where an out-of-band
//! planning call costs a whole round trip — 236 ms of TTFT at best, per
//! `serving_latency_probe`.
//!
//! # The question that decides the shape
//!
//! Not "do filters help when they are right" — obviously they do. The question
//! is what they cost when they are wrong, because a model writing filters will
//! be wrong sometimes and a filter is the one retrieval signal that can remove
//! the answer rather than rank it lower.
//!
//! This is not hypothetical here. `corpus::PROBES` records a live failure of
//! exactly that shape: asked where they were collecting Priya's cake, the model
//! wrote `recall_context({"query": "Priya's cake collection location"})` and
//! got five records whose *subject* is Priya — her hairdresser, her restaurant,
//! what she drinks — while the commitment, which merely mentions her, ranked
//! below all of them. A DSL that separates "whose fact is this" from "who does
//! it mention" addresses that directly. A DSL that turns the same confusion
//! into a hard filter makes it unrecoverable.
//!
//! So every condition below is run twice: once with a filter derived from the
//! record that actually answers the question, and once with a filter that is
//! confidently wrong.
//!
//! # What is measured
//!
//! Three filters, of decreasing realism:
//!
//! - **`about`** — the subject. A model can infer "is this about me or about
//!   Rhea" from the conversation about as reliably as it infers anything.
//! - **`about` + `kind`** — also inferable, and the kind vocabulary is small.
//! - **`about` + `attribute`** — the exact predicate. Reported with its
//!   survivor count, because a filter that leaves four records out of 1,199 is
//!   a ceiling and not a plan.
//!
//! Two ways of applying them:
//!
//! - **hard** — restrict the candidate set, then fuse over survivors;
//! - **soft** — the filtered ranking joins the fusion as one more opinion,
//!   alongside the unfiltered lexical and semantic rankings.
//!
//! # What it found
//!
//! | configuration | top-1 | top-5 | MRR | lost | survivors |
//! |---|---|---|---|---|---|
//! | no filter (current best) | 64/93 | 79/93 | 0.752 | 6/93 | 1199 |
//! | `about`, correct, hard | 73/93 | 81/93 | 0.829 | 4/93 | 487 |
//! | `about`, correct, **soft** | 74/93 | 82/93 | 0.833 | 4/93 | 487 |
//! | `about`, **wrong**, hard | 0/93 | **0/93** | 0.000 | **93/93** | 164 |
//! | `about`, **wrong**, soft | 63/93 | 77/93 | 0.737 | 6/93 | 164 |
//! | `about`+`kind`, correct, hard | 84/93 | 90/93 | 0.936 | 2/93 | 104 |
//! | **`about`+`kind`, correct, soft** | 83/93 | **90/93** | 0.928 | 2/93 | 104 |
//! | `about`+`kind`, **wrong**, hard | 0/93 | **0/93** | 0.000 | **93/93** | 48 |
//! | `about`+`kind`, **wrong**, soft | 64/93 | 77/93 | 0.745 | 6/93 | 48 |
//! | `about`+`attribute`, correct, hard | 88/93 | 91/93 | 0.963 | 1/93 | 22 |
//! | `about`+`attribute`, correct, soft | 85/93 | 91/93 | 0.945 | 1/93 | 22 |
//! | `about`+`attribute`, **wrong**, soft | 64/93 | 78/93 | 0.747 | 6/93 | 1 |
//!
//! **A wrong hard filter loses everything, every time.** Not "degrades" —
//! 0 of 93, with the answer absent from the results entirely in all 93 cases,
//! for every filter shape. That is what a hard filter *is*: it removes the
//! answer, and no amount of downstream ranking can put back what was never a
//! candidate.
//!
//! **A wrong soft filter costs one or two questions.** 77 or 78 against the
//! unfiltered 79, because the unfiltered lexical and semantic rankings are
//! still in the fusion and still carry the answer.
//!
//! **A correct filter is worth a lot.** `about`+`kind` takes top-5 from 79 to
//! 90 and MRR from 0.752 to 0.93 — bigger than the gain from adding embeddings
//! in the first place.
//!
//! # The number the design turns on
//!
//! Since the model will sometimes be wrong, what matters is how often it must
//! be right for filtering to beat not filtering. Solving
//! `p·right + (1−p)·wrong = baseline`:
//!
//! | filter | hard | soft |
//! |---|---|---|
//! | `about` | 98% | 40% |
//! | `about` + `kind` | 88% | **15%** |
//! | `about` + `attribute` | 87% | **8%** |
//!
//! Hard filtering needs the model to be right about nine times in ten before it
//! breaks even — and it is being asked to infer whose fact this is from an
//! utterance like "the drink I always end up with". Soft filtering with
//! `about`+`kind` breaks even at fifteen percent, which is a bar you would have
//! to work at to miss.
//!
//! # So: the recommendation
//!
//! Add the fields, and never let them gate. A filter becomes one more ranking
//! in the fusion — the same `1/(60 + rank)` everything else uses — so the
//! filtered view competes with the unfiltered ones instead of replacing them.
//!
//! That is the third independent measurement in this crate pointing the same
//! way. `needs_semantic_fallback` gated the semantic backend and cost 80% of
//! its value; `satisfies` gated the prepared snapshot and discarded 65 of 93
//! correct ones; and a filter gate here costs everything. Retrieval signals in
//! this engine should rank, not exclude.
//!
//! Reads the embeddings `semantic_fusion_probe` cached; skips without them.

#![cfg(feature = "gemini-llm")]

mod common;

use std::collections::HashMap;

use common::corpus::{self, PROBES};
use common::paraphrase;
use common::rank::{CANDIDATES, as_hits, fuse, lexical, rank_of};
use common::views::structural_view;
use common::{ScratchDir, file_backed_engine};

use gemini_memory_rs::bm25::{IndexedMemory, MemoryIndex, SearchHit};
use gemini_memory_rs::core::{CanonicalMemory, MemoryId, MemoryKind, MemoryStatus, stable_hash};

const EMBEDDING_MODEL: &str = "gemini-embedding-2";
const WIDTH: usize = 768;
const CACHE: &str = "semantic-width-embeddings.json";

/// The closed vocabulary a model would be given.
///
/// Every field maps onto something already in the frontmatter and already
/// indexed, and every one is a flat scalar or a list of scalars — which is not
/// an aesthetic choice. Narrowing the derived JSON Schema to the API's subset
/// inlines subschemas and flattens `oneOf`-of-`enum`, so a nested or tagged DSL
/// arrives at the model as something other than what was written. Anything the
/// model must understand has to survive that, which in practice means: flat
/// fields, plain enums, and the explanation in the *field* description.
#[derive(Default, Clone, Debug)]
struct Filter {
    /// Whose fact this is — the subject surface form.
    about: Option<String>,
    /// What attribute of them — the canonical predicate.
    attribute: Option<String>,
    /// Who else it has to mention. Deliberately distinct from `about`: the
    /// difference between them is the `errand` probe's live failure.
    mentions: Vec<String>,
    /// What sort of memory.
    kind: Option<MemoryKind>,
}

impl Filter {
    fn matches(&self, memory: &CanonicalMemory) -> bool {
        if let Some(about) = &self.about {
            if !memory.retrieval.subject.eq_ignore_ascii_case(about) {
                return false;
            }
        }
        if let Some(attribute) = &self.attribute {
            if memory.predicate.as_str() != attribute {
                return false;
            }
        }
        if let Some(kind) = &self.kind {
            if memory.kind != *kind {
                return false;
            }
        }
        for entity in &self.mentions {
            if !memory
                .retrieval
                .entities
                .iter()
                .any(|e| e.eq_ignore_ascii_case(entity))
            {
                return false;
            }
        }
        true
    }
}

#[derive(Default, Clone, Copy)]
struct Tally {
    asked: usize,
    first: usize,
    top_five: usize,
    reciprocal: f64,
    /// Times the answer was not in the candidate set at all.
    lost: usize,
    survivors: usize,
}

impl Tally {
    fn record(&mut self, rank: Option<usize>, survivors: usize) {
        self.asked += 1;
        self.survivors += survivors;
        match rank {
            Some(0) => {
                self.first += 1;
                self.top_five += 1;
                self.reciprocal += 1.0;
            }
            Some(r) => {
                self.reciprocal += 1.0 / (r + 1) as f64;
                if r < 5 {
                    self.top_five += 1;
                }
            }
            None => self.lost += 1,
        }
    }

    fn row(&self, label: &str) -> String {
        format!(
            "{label:<34} {:<9} {:<9} {:<8.3} {:<9} {}\n",
            format!("{}/{}", self.first, self.asked),
            format!("{}/{}", self.top_five, self.asked),
            self.reciprocal / self.asked.max(1) as f64,
            format!("{}/{}", self.lost, self.asked),
            self.survivors / self.asked.max(1),
        )
    }
}

fn cached() -> Option<HashMap<String, Vec<f32>>> {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(CACHE);
    serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()
}

fn key(task: &str, text: &str) -> String {
    stable_hash(&format!("{EMBEDDING_MODEL}|{WIDTH}|{task}|{text}"))
}

fn semantic_over(query: &[f32], vectors: &[(usize, &Vec<f32>)], limit: usize) -> Vec<(usize, f32)> {
    let mut scored: Vec<(usize, f32)> = vectors
        .iter()
        .map(|(i, v)| (*i, v.iter().zip(query).map(|(a, b)| a * b).sum::<f32>()))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    scored
}

#[tokio::test]
async fn what_a_model_written_filter_would_buy_and_what_it_risks() {
    let Some(cache) = cached() else {
        eprintln!(
            "SKIP what_a_model_written_filter_would_buy_and_what_it_risks: no embedding \
             cache at {CACHE}. Run `semantic_fusion_probe` first."
        );
        return;
    };

    let scratch = ScratchDir::new("filter-dsl");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let records = corpus::installed(&engine).await;
    let active: Vec<&CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();

    let mut vectors = Vec::with_capacity(active.len());
    for memory in &active {
        let Some(vector) = cache.get(&key("RETRIEVAL_DOCUMENT", &structural_view(memory))) else {
            eprintln!("SKIP: cache is missing the structural view at {WIDTH}d.");
            return;
        };
        vectors.push(vector.clone());
    }

    let index = MemoryIndex::build(active.iter().map(|m| IndexedMemory::from_canonical(m)));
    let ids: Vec<MemoryId> = active.iter().map(|m| m.id.clone()).collect();

    let questions: Vec<(&'static str, &'static str)> = paraphrase::all()
        .map(|(probe, phrasing)| (probe, phrasing.query))
        .collect();

    // Conditions: how the filter is built, and whether it is right.
    #[derive(Clone, Copy, PartialEq)]
    enum Shape {
        About,
        AboutKind,
        AboutAttribute,
    }
    let shapes = [
        ("about", Shape::About),
        ("about + kind", Shape::AboutKind),
        ("about + attribute", Shape::AboutAttribute),
    ];

    let mut baseline = Tally::default();
    let mut results: Vec<(String, Tally, Tally, Tally, Tally)> = Vec::new();

    // Baseline first: the configuration the experiments already recommend.
    for (i, (_, question)) in questions.iter().enumerate() {
        let probe = PROBES
            .iter()
            .find(|p| p.name == questions[i].0)
            .expect("probe");
        let Some(query_vector) = cache.get(&key("RETRIEVAL_QUERY", question)) else {
            eprintln!("SKIP: cache is missing query vectors.");
            return;
        };
        let all: Vec<(usize, &Vec<f32>)> = vectors.iter().enumerate().collect();
        let sem = as_hits(&semantic_over(query_vector, &all, CANDIDATES), &ids);
        let lex = lexical(&index, question);
        let fused = fuse(&[&lex, &sem, &sem]);
        baseline.record(rank_of(&fused, probe.target), active.len());
    }

    for (label, shape) in shapes {
        let (mut hard_right, mut soft_right) = (Tally::default(), Tally::default());
        let (mut hard_wrong, mut soft_wrong) = (Tally::default(), Tally::default());

        for (i, (_, question)) in questions.iter().enumerate() {
            let probe = PROBES
                .iter()
                .find(|p| p.name == questions[i].0)
                .expect("probe");
            let target = active
                .iter()
                .position(|m| m.id.as_str() == probe.target)
                .expect("target in corpus");
            let query_vector = cache
                .get(&key("RETRIEVAL_QUERY", question))
                .expect("query vector");

            let build = |memory: &CanonicalMemory| -> Filter {
                let mut filter = Filter {
                    about: Some(memory.retrieval.subject.clone()),
                    ..Default::default()
                };
                match shape {
                    Shape::About => {}
                    Shape::AboutKind => filter.kind = Some(memory.kind),
                    Shape::AboutAttribute => {
                        filter.attribute = Some(memory.predicate.as_str().to_string())
                    }
                }
                filter
            };

            // Right: derived from the record that answers the question.
            //
            // Wrong: derived from a record that differs on *every* field the
            // filter uses. Picking "some other probe's target" is not enough —
            // most probes are about the user and most are preferences, so a
            // filter built from one of those is frequently correct by accident
            // and the wrong-filter column comes out flatteringly high.
            let right = build(active[target]);
            let truth = active[target];
            let other = active
                .iter()
                .find(|m| {
                    m.retrieval.subject != truth.retrieval.subject
                        && m.kind != truth.kind
                        && m.predicate != truth.predicate
                })
                .copied()
                .expect("a record differing on every filtered field");
            let wrong = build(other);

            let lex = lexical(&index, question);
            let all: Vec<(usize, &Vec<f32>)> = vectors.iter().enumerate().collect();
            let sem = as_hits(&semantic_over(query_vector, &all, CANDIDATES), &ids);

            for (filter, hard, soft) in [
                (&right, &mut hard_right, &mut soft_right),
                (&wrong, &mut hard_wrong, &mut soft_wrong),
            ] {
                let surviving: Vec<(usize, &Vec<f32>)> = vectors
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| filter.matches(active[*i]))
                    .collect();
                let survivors = surviving.len();

                // Hard: the filter removes candidates outright.
                let filtered_sem =
                    as_hits(&semantic_over(query_vector, &surviving, CANDIDATES), &ids);
                let kept: Vec<SearchHit> = lex
                    .iter()
                    .filter(|h| {
                        active
                            .iter()
                            .find(|m| m.id == h.id)
                            .is_some_and(|m| filter.matches(m))
                    })
                    .cloned()
                    .collect();
                let hard_fused = fuse(&[&kept, &filtered_sem, &filtered_sem]);
                hard.record(rank_of(&hard_fused, probe.target), survivors);

                // Soft: the filtered ranking is one more opinion, and the
                // unfiltered rankings still stand behind it.
                let soft_fused = fuse(&[&lex, &sem, &sem, &filtered_sem, &filtered_sem]);
                soft.record(rank_of(&soft_fused, probe.target), survivors);
            }
        }
        results.push((
            label.to_string(),
            hard_right,
            soft_right,
            hard_wrong,
            soft_wrong,
        ));
    }

    let header = format!(
        "{:<34} {:<9} {:<9} {:<8} {:<9} {}\n",
        "configuration", "top-1", "top-5", "MRR", "lost", "survivors"
    );
    let mut report = format!(
        "\nwhat a model-written filter buys, and what it risks\n\
         {} questions over {} records; `lost` is the answer missing from the results \
         entirely\n\n{header}",
        questions.len(),
        active.len(),
    );
    report.push_str(&baseline.row("no filter (current best)"));
    report.push('\n');
    for (label, hard_right, soft_right, hard_wrong, soft_wrong) in &results {
        report.push_str(&hard_right.row(&format!("{label}, correct, hard")));
        report.push_str(&soft_right.row(&format!("{label}, correct, soft")));
        report.push_str(&hard_wrong.row(&format!("{label}, WRONG, hard")));
        report.push_str(&soft_wrong.row(&format!("{label}, WRONG, soft")));
        report.push('\n');
    }
    // The number that decides the design: how often would the model have to
    // get the filter right for the strategy to beat not filtering at all?
    //
    //   p·right + (1−p)·wrong = baseline   ⇒   p = (baseline − wrong) / (right − wrong)
    //
    // Below that accuracy the filter is a liability; above it, an asset.
    report.push_str(
        "break-even: how often the model must get the filter right for it to beat \
         no filter\n\n",
    );
    for (label, hard_right, soft_right, hard_wrong, soft_wrong) in &results {
        for (mode, right, wrong) in [
            ("hard", hard_right, hard_wrong),
            ("soft", soft_right, soft_wrong),
        ] {
            let (right, wrong) = (right.top_five as f64, wrong.top_five as f64);
            let base = baseline.top_five as f64;
            let verdict = if right <= wrong {
                "never worth it".to_string()
            } else {
                let p = ((base - wrong) / (right - wrong)).clamp(0.0, 1.0);
                format!("{:.0}% accurate", p * 100.0)
            };
            report.push_str(&format!("  {label:<20} {mode:<6} {verdict}\n"));
        }
    }
    eprintln!("{report}");

    // The finding the design rests on: a wrong filter applied hard destroys the
    // result, and applied soft costs nothing. If that ever stops being true the
    // recommendation should change with it.
    for (label, _, _, hard_wrong, soft_wrong) in &results {
        assert!(
            soft_wrong.top_five > hard_wrong.top_five,
            "with a wrong `{label}` filter, hard filtering answered {} of {} and soft \
             filtering {} — soft filtering is supposed to be the one that survives being \
             wrong\n{report}",
            hard_wrong.top_five,
            hard_wrong.asked,
            soft_wrong.top_five,
        );
        assert!(
            soft_wrong.top_five * 10 >= baseline.top_five * 9,
            "a wrong `{label}` filter cost soft fusion {} of {} against the unfiltered \
             baseline's {} — soft is only worth recommending while being wrong is nearly \
             free\n{report}",
            soft_wrong.top_five,
            soft_wrong.asked,
            baseline.top_five,
        );
    }
}
