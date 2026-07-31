//! The query set that measures paraphrase tolerance, shared by the tests that
//! need it.
//!
//! Kept in one place because two suites ask the same questions for different
//! reasons: `memory_paraphrase` measures what lexical retrieval alone does with
//! them, and `semantic_fusion_probe` measures what a semantic layer adds. A
//! divergence between the two query sets would make the comparison meaningless.
//!
//! # Two axes, because they measure different things
//!
//! [`Tier`] is **difficulty** — how far the question sits from the words of the
//! record that answers it. It predicts lexical recall almost perfectly, because
//! word overlap is the only signal BM25 has.
//!
//! [`Mode`] is **what the person is doing**, and it is the axis a product cares
//! about. Somebody wearing glasses does not mostly ask well-formed questions.
//! They give instructions with their hands full, point at things, check whether
//! they have already done something, trail off mid-sentence, and switch
//! language mid-clause. Those produce very different query strings, and a
//! retriever that only handles the well-formed ones works in a demo and not on
//! a face.
//!
//! Every query carries both, so the report can be cut either way: by how hard
//! it is, and by what kind of moment produced it.

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
    pub const COUNT: usize = 5;

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).expect("tier")
    }

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

/// What the person was doing when they said it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// A well-formed question. The case every demo shows.
    Question,
    /// Hands full, eyes elsewhere: an instruction rather than an enquiry. The
    /// answer is never spoken back, so a wrong retrieval becomes a wrong
    /// *action* instead of a wrong sentence.
    Command,
    /// Said while looking at something — "this", "that place", "them". The
    /// referent is in the world rather than in the sentence.
    InSitu,
    /// Forward-looking, and often naming nothing that is in the corpus: the
    /// question is about an occasion, the answer is a stored preference.
    Planning,
    /// Checking whether something is already dealt with. A miss here reads as
    /// "you never told me", which is the accusation a memory product least
    /// survives.
    Verify,
    /// Clipped, trailing or interrupted — how people talk to a device they are
    /// not looking at.
    Terse,
    /// Hindi–English mixing, ordinary speech for the market this is built for,
    /// and it produces queries with almost no English content words.
    CodeSwitched,
    /// Names an *event* and expects an attribute back: "anniversary dinner",
    /// "we're having people over". The corpus holds no record about the
    /// occasion — the link from occasion to preference is the retrieval.
    Occasion,
    /// States a restriction and expects the fact behind it: "somewhere I can
    /// actually eat". The answer is what makes the constraint true.
    Constraint,
    /// Picks one out of a set: "which of us can't have sesame". The answer is a
    /// record whose distinguishing value the question already names.
    Comparative,
    /// Anchored in time rather than in topic: "what did I say I'd do this
    /// weekend". Needs the temporal window a plan computes and then discards.
    Temporal,
    /// Points at a thing by history rather than name: "that place we went to",
    /// "the gym I joined".
    Referential,
}

impl Mode {
    pub const ALL: [Mode; 12] = [
        Mode::Question,
        Mode::Command,
        Mode::InSitu,
        Mode::Planning,
        Mode::Verify,
        Mode::Terse,
        Mode::CodeSwitched,
        Mode::Occasion,
        Mode::Constraint,
        Mode::Comparative,
        Mode::Temporal,
        Mode::Referential,
    ];
    pub const COUNT: usize = 12;

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|m| *m == self).expect("mode")
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Question => "question",
            Mode::Command => "command",
            Mode::InSitu => "in-situ",
            Mode::Planning => "planning",
            Mode::Verify => "verify",
            Mode::Terse => "terse",
            Mode::CodeSwitched => "code-switched",
            Mode::Occasion => "occasion",
            Mode::Constraint => "constraint",
            Mode::Comparative => "comparative",
            Mode::Temporal => "temporal",
            Mode::Referential => "referential",
        }
    }
}

/// One way of asking for one fact.
pub struct Phrasing {
    /// How far it sits from the record's own words.
    pub tier: Tier,
    /// What the person was doing when they said it.
    pub mode: Mode,
    /// What they said.
    pub query: &'static str,
}

/// Every way of asking for one needle.
pub struct Phrasings {
    /// The needle these ask about, by probe name.
    pub probe: &'static str,
    /// The phrasings.
    pub queries: &'static [Phrasing],
}

/// Shorthand, so the table below reads as data rather than as constructors.
const fn p(tier: Tier, mode: Mode, query: &'static str) -> Phrasing {
    Phrasing { tier, mode, query }
}

use Mode::{
    CodeSwitched, Command, Comparative, Constraint, InSitu, Occasion, Planning, Question,
    Referential, Temporal, Terse, Verify,
};
use Tier::{Direct, Echo, Indirect, Inferential, Synonym};

/// The query set.
///
/// Written from the question side — what somebody talking to a pair of glasses
/// would actually say — rather than by mutating the record, which is the only
/// way to keep the harder tiers honest.
pub const PHRASINGS: &[Phrasings] = &[
    Phrasings {
        probe: "coffee_order",
        queries: &[
            p(Indirect, Occasion, "I'm doing a coffee run for the office"),
            p(Inferential, Constraint, "nothing too milky, what suits me"),
            p(Direct, Comparative, "which of us drinks a cortado"),
            p(Indirect, Referential, "the drink I always end up with"),
            p(Echo, Question, "the user's usual coffee order"),
            p(Direct, Question, "what coffee do I usually order"),
            p(Synonym, Question, "my go-to drink at a cafe"),
            p(
                Indirect,
                InSitu,
                "I'm at the counter, what do I normally get",
            ),
            p(Inferential, Command, "order me the same as always"),
            p(Direct, Command, "get my usual coffee"),
            p(Direct, Terse, "my coffee, the usual one"),
            p(
                Inferential,
                InSitu,
                "they're asking what I want, what do I say",
            ),
            p(Direct, CodeSwitched, "mera usual coffee order kya hai"),
        ],
    },
    Phrasings {
        probe: "allergy",
        queries: &[
            p(
                Indirect,
                Occasion,
                "we're having people over, anything to avoid",
            ),
            p(Indirect, Constraint, "somewhere I can actually eat"),
            p(Direct, Comparative, "which of us can't have sesame"),
            p(Inferential, Referential, "that thing that makes me ill"),
            p(Echo, Question, "the user's food allergy"),
            p(Direct, Question, "what food am I allergic to"),
            p(Synonym, Question, "which ingredient do I have to avoid"),
            p(Indirect, InSitu, "is a tahini dressing safe for me"),
            p(
                Inferential,
                InSitu,
                "can I eat this bagel with seeds on top",
            ),
            p(Direct, Verify, "am I allergic to anything"),
            p(Direct, Terse, "allergies, what were they"),
            p(
                Inferential,
                Planning,
                "booking dinner, anything the kitchen should know",
            ),
            p(Direct, CodeSwitched, "mujhe kis cheez se allergy hai"),
        ],
    },
    Phrasings {
        probe: "spouse_restaurant",
        queries: &[
            p(
                Indirect,
                Occasion,
                "anniversary dinner, where should I book",
            ),
            p(Indirect, Constraint, "a place that works for Rhea"),
            p(
                Inferential,
                Referential,
                "that restaurant we went to for her birthday",
            ),
            p(
                Indirect,
                Temporal,
                "where did we eat the last time she chose",
            ),
            p(Echo, Question, "Rhea's favourite restaurant"),
            p(Direct, Question, "where does Rhea like to eat"),
            p(
                Synonym,
                Question,
                "which place does my wife enjoy dining at",
            ),
            p(
                Indirect,
                Planning,
                "book somewhere my wife will be happy with",
            ),
            p(
                Inferential,
                Question,
                "she chose the venue last time, which was it",
            ),
            p(Direct, Command, "book Rhea's favourite place"),
            p(Direct, Terse, "that restaurant Rhea likes"),
            p(Indirect, Planning, "anniversary dinner, where should we go"),
        ],
    },
    Phrasings {
        probe: "gift_idea",
        queries: &[
            p(Indirect, Occasion, "her birthday is next month, ideas"),
            p(Inferential, Constraint, "something she'd actually use"),
            p(Inferential, Referential, "the thing she keeps mentioning"),
            p(Direct, Temporal, "what has Rhea been hinting at lately"),
            p(
                Echo,
                Question,
                "what Rhea wants for her birthday, gift idea",
            ),
            p(Direct, Question, "what should I get Rhea for her birthday"),
            p(Synonym, Question, "any present ideas for my wife"),
            p(
                Indirect,
                Planning,
                "her birthday is coming up and I'm stuck",
            ),
            p(
                Inferential,
                Question,
                "she keeps dropping hints, about what",
            ),
            p(Direct, InSitu, "I'm in a shop, what was Rhea after"),
            p(Synonym, Terse, "Rhea's present, what was it"),
        ],
    },
    Phrasings {
        probe: "climbing_gym",
        queries: &[
            p(Indirect, Occasion, "free evening, fancy a climb"),
            p(Inferential, Referential, "the place I joined for climbing"),
            p(Direct, Comparative, "which of my gyms is the climbing one"),
            p(Indirect, Temporal, "where have I been training this year"),
            p(Echo, Question, "the user's climbing gym"),
            p(Direct, Question, "which gym do I climb at"),
            p(Synonym, Question, "where do I train on the wall"),
            p(
                Indirect,
                Planning,
                "I want to get a session in this evening, where do I go",
            ),
            p(Inferential, Question, "remind me where my membership is"),
            p(Direct, Command, "navigate to my climbing gym"),
            p(Synonym, Terse, "the bouldering place, name"),
        ],
    },
    Phrasings {
        probe: "corrected_barber",
        queries: &[
            p(Indirect, Occasion, "wedding next week, I need a cut"),
            p(
                Inferential,
                Referential,
                "the person who did my hair last time",
            ),
            p(Direct, Temporal, "who has been cutting my hair recently"),
            p(Inferential, Constraint, "someone who knows how I like it"),
            p(Echo, Question, "the user's barber"),
            p(Direct, Question, "who cuts my hair"),
            p(Synonym, Question, "who is my hairdresser"),
            p(Indirect, Planning, "I need a trim, who do I book with"),
            p(Inferential, Question, "who do I go to for a fade"),
            p(Direct, Command, "call whoever does my hair"),
            p(Synonym, Terse, "haircut, who"),
        ],
    },
    Phrasings {
        probe: "errand",
        queries: &[
            p(Direct, Temporal, "what did I say I'd do this weekend"),
            p(Direct, Temporal, "when is the cake pickup"),
            p(Inferential, Referential, "the bakery for Priya's thing"),
            p(Indirect, Occasion, "Priya's celebration, what's my job"),
            p(
                Echo,
                Question,
                "where the user promised to collect Priya's cake",
            ),
            p(Direct, Question, "where am I picking up Priya's cake"),
            p(
                Synonym,
                Question,
                "which shop has my sister's order waiting",
            ),
            p(
                Indirect,
                Verify,
                "what do I have to do for Priya on Saturday",
            ),
            p(
                Inferential,
                Verify,
                "I have an errand this weekend, remind me",
            ),
            p(Direct, Verify, "have I still got to collect that cake"),
            p(Direct, Terse, "Priya's cake, where from"),
        ],
    },
    Phrasings {
        probe: "possession",
        queries: &[
            p(Inferential, Referential, "the bike I bought last year"),
            p(Direct, Comparative, "which of us rides a Thornbury"),
            p(Indirect, Constraint, "spares that fit what I ride"),
            p(Indirect, Occasion, "weekend ride, what am I taking"),
            p(Echo, Question, "the user's bicycle"),
            p(Direct, Question, "what bike do I ride"),
            p(Synonym, Question, "what make is my cycle"),
            p(
                Indirect,
                InSitu,
                "I'm booking a service, which model is mine",
            ),
            p(Inferential, Question, "what am I riding to work on"),
            p(Direct, Command, "look up spares for my bike"),
            p(Synonym, Terse, "my bike, make"),
        ],
    },
];

/// Every query in the set, paired with the probe it belongs to.
pub fn all() -> impl Iterator<Item = (&'static str, &'static Phrasing)> {
    PHRASINGS
        .iter()
        .flat_map(|set| set.queries.iter().map(move |q| (set.probe, q)))
}

/// How many questions the set holds.
pub fn count() -> usize {
    PHRASINGS.iter().map(|set| set.queries.len()).sum()
}

/// What fraction of a question's content words appear in the text that answers
/// it.
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
