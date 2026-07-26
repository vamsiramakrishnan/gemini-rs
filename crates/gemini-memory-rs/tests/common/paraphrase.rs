//! The query set that measures paraphrase tolerance, shared by the tests that
//! need it.
//!
//! Kept in one place because two suites ask the same questions for different
//! reasons: `memory_paraphrase` measures what lexical retrieval alone can do
//! with them, and `semantic_fusion_probe` measures what a semantic layer adds.
//! A divergence between the two query sets would make the comparison
//! meaningless.

#![allow(dead_code)]

use std::collections::HashSet;

use gemini_memory_rs::bm25::tokenize;

/// How far a question sits from the wording of the record that answers it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
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
    pub const ALL: [Tier; 5] = [
        Tier::Echo,
        Tier::Direct,
        Tier::Synonym,
        Tier::Indirect,
        Tier::Inferential,
    ];

    pub fn label(self) -> &'static str {
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
pub struct Phrasings {
    /// The needle these ask about, by probe name.
    pub probe: &'static str,
    /// One phrasing per tier, in [`Tier::ALL`] order.
    pub queries: [&'static str; 5],
}

pub const PHRASINGS: &[Phrasings] = &[
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
pub fn overlap(query: &str, statement: &str) -> f64 {
    let asked: HashSet<String> = tokenize(query).into_iter().collect();
    if asked.is_empty() {
        return 0.0;
    }
    let held: HashSet<String> = tokenize(statement).into_iter().collect();
    asked.iter().filter(|t| held.contains(*t)).count() as f64 / asked.len() as f64
}
