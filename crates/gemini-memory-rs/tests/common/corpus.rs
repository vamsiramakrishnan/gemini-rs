//! A synthetic memory corpus for a consumer assistant that lives in a pair of
//! smart glasses, sized on demand.
//!
//! # What it is
//!
//! Everything such an assistant would have picked up from a person's ordinary
//! life across months of conversation: the people around them and what those
//! people like, the places they go, what they order, what they own, what they
//! have promised to do, where they have been, and what happened last week.
//! Twelve generators produce it, cycling so that the vocabulary keeps varying,
//! and [`generate`] will emit as many records as a caller asks for.
//!
//! # Why it is large
//!
//! Retrieval only becomes a question at scale. With a dozen facts in the index,
//! a query that matches anything matches the right thing, and every ranking bug
//! is invisible. At a thousand, most questions have dozens of records that look
//! like plausible answers to them — several dozen "X's usual coffee is Y", a
//! drawerful of places on the same street, a family's worth of birthdays — and
//! the engine has to be right rather than lucky.
//!
//! # The needles
//!
//! A fixed set of facts sits on top of the generated bulk, each carrying an
//! answer token that appears **exactly once in the whole corpus** and cannot be
//! guessed from world knowledge. That is what makes an assertion cheap — one
//! word of the answer decides it — while leaving the underlying job hard.
//!
//! # The traps
//!
//! A needle with no near-miss is an easy problem, so each one is given a
//! deliberate competitor: the same fact about somebody else, the same topic in
//! an episode rather than a preference, a superseded value still on disk, a
//! second person doing the same errand somewhere else. Every trap's
//! distinguishing token is in the probe's `forbid` list, so retrieving the
//! wrong record fails by name instead of passing as close enough.

#![allow(dead_code)]

use chrono::{Duration, Utc};

use gemini_memory_rs::core::{
    CanonicalMemory, CanonicalPredicate, EntityRef, EvidenceCounters, Explicitness, MemoryId,
    MemoryKind, MemorySource, MemoryStatus, MemoryValue, PrivacyMetadata, RetrievalMetadata,
    SessionId, TemporalMetadata, TemporalScope, TurnId, UserId, normalize_token,
};
use gemini_memory_rs::engine::MemoryEngine;
use gemini_memory_rs::okf::MemoryTransaction;

/// The corpus size the quality tests use.
///
/// Chosen so every probe has dozens of same-shaped competitors rather than a
/// handful, which is the point at which ranking starts to matter.
pub const DEFAULT_SIZE: usize = 1_200;

// ─── probes ─────────────────────────────────────────────────────────────────

/// One question, and what a correct answer to it looks like.
pub struct Probe {
    /// Case name, for failure messages and the report.
    pub name: &'static str,
    /// What the user says out loud, verbatim.
    pub ask: &'static str,
    /// What a model would plausibly pass to `recall_context` for that question.
    ///
    /// Distinct from [`Probe::ask`] on purpose: the tool's `query` argument is
    /// written by the model, not lifted from the transcript, so measuring
    /// retrieval with the raw utterance — "answer in five words" and all —
    /// measures something no deployment does.
    pub query: &'static str,
    /// The record that has to come back for the answer to be knowledge rather
    /// than a guess.
    pub target: &'static str,
    /// Any one of these in the answer means the needle was found.
    pub expect: &'static [&'static str],
    /// None of these may appear: each belongs to a trap a weaker ranker would
    /// have surfaced instead.
    pub forbid: &'static [&'static str],
    /// Why this probe is not asserted over the wire, when it is not.
    ///
    /// The offline tests drive [`Probe::query`], which is fixed. The live tests
    /// drive whatever the model writes, which is not — so a probe whose live
    /// outcome turns on a known engine limitation fails intermittently, for a
    /// reason the live test is not about. Such a probe is asserted offline,
    /// skipped live, and the limitation gets its own named test.
    pub live_gap: Option<&'static str>,
}

/// The probe set: eight everyday questions, each with a different way to be
/// wrong.
pub const PROBES: &[Probe] = &[
    Probe {
        // Dozens of people in the corpus have a usual order. Only the user's is
        // a cortado, and an episode about buying cold brew for the office is
        // the closest match to the question that is not the answer.
        name: "coffee_order",
        ask: "What's my usual coffee order? Answer with just the drink.",
        query: "the user's usual coffee order",
        target: "mem_user_coffee",
        expect: &["cortado"],
        forbid: &["cold brew", "flat white", "americano", "cappuccino"],
        live_gap: None,
    },
    Probe {
        // The user is allergic to sesame; forty other people are allergic to
        // something else, and an episode records somebody reacting to peanuts
        // at the user's own dinner.
        name: "allergy",
        ask: "What am I allergic to? Answer with just the food.",
        query: "the user's food allergy",
        target: "mem_user_allergy",
        expect: &["sesame"],
        forbid: &["peanut", "peanuts", "shellfish", "gluten", "kiwi"],
        live_gap: None,
    },
    Probe {
        // Same predicate as dozens of records, and the user has a favourite
        // place of their own. Answering with the user's own is the mistake.
        name: "spouse_restaurant",
        ask: "Where does Rhea like to eat? Answer with just the name.",
        query: "Rhea's favourite restaurant",
        target: "mem_rhea_restaurant",
        expect: &["fennelmark"],
        forbid: &["bellagrove", "harrowgate", "silverbeam"],
        live_gap: None,
    },
    Probe {
        // A gift the assistant is meant to remember for a birthday — the exact
        // errand a consumer device is bought for. The user's own wishlist is
        // the trap.
        name: "gift_idea",
        ask: "What has Rhea been hinting at for her birthday? Answer with just the item.",
        query: "what Rhea wants for her birthday, gift idea",
        target: "mem_rhea_gift",
        expect: &["skylark"],
        forbid: &["tarnhelm", "windrose"],
        live_gap: None,
    },
    Probe {
        // Three venues, one of which is the answer, one of which shares the
        // predicate exactly.
        name: "climbing_gym",
        ask: "Which gym do I climb at? Answer with just the name.",
        query: "the user's climbing gym",
        target: "mem_user_gym",
        expect: &["karvala"],
        forbid: &["ridgeline", "mirador"],
        live_gap: None,
    },
    Probe {
        // The superseded record is still on disk and must never reach the
        // model.
        name: "corrected_barber",
        ask: "Who cuts my hair these days? Answer with just the name.",
        query: "the user's barber, who cuts their hair",
        target: "mem_barber_now",
        expect: &["deepa"],
        forbid: &["girish", "anisha", "kabini"],
        live_gap: None,
    },
    Probe {
        // Somebody else is running the same errand somewhere else, so "collect
        // Saturday bakery" matches the trap as strongly as the needle.
        name: "errand",
        ask: "Where am I collecting Priya's cake from? Answer with just the name.",
        query: "where the user promised to collect Priya's cake",
        target: "mem_commitment_cake",
        expect: &["ashgrove"],
        forbid: &["windrose", "marlowe"],
        live_gap: Some(
            "naming a person in a query outweighs saying what you want to know about \
             them. Asked where they were collecting Priya's cake, the model wrote \
             `recall_context({\"query\": \"Priya's cake collection location\"})` and got \
             five records whose *subject* is Priya — her hairdresser, her restaurant, \
             what she drinks — while the commitment, which merely *mentions* her, \
             ranked below all of them. The cause is field-length normalization: a \
             subject field holds one token and scores high, whereas the mentioned \
             entity lands in an `entities` field alongside the owner's own surface \
             forms and is normalized down. Asserted offline, where the query is \
             fixed; see `a_subject_less_recall_does_not_default_to_the_user` for the \
             same weakness from the other direction.",
        ),
    },
    Probe {
        // A possession, asked the way someone asks their glasses: the model has
        // to distinguish the user's own bike from the two they talked about.
        name: "possession",
        ask: "What bike do I ride? Answer with just the make.",
        query: "the user's bicycle",
        target: "mem_user_bike",
        expect: &["thornbury"],
        forbid: &["castellane", "idlewood"],
        live_gap: None,
    },
];

/// Questions the corpus genuinely cannot answer.
///
/// Every one of them is phrased the way a model writes a memory lookup — third
/// person, about "the user" — because that phrasing is the interesting part:
/// the corpus's own subject form appears in most records, in the
/// highest-weighted field, so a query carrying it can match everything while
/// discriminating nothing.
pub const UNANSWERABLE: &[&str] = &[
    "the user's mortgage interest rate",
    "the user's blood type",
    "what the user thinks about quantum computing",
    "the user's national insurance number",
];

/// Whether `text` says `phrase`, comparing whole words.
///
/// Substring matching is not good enough: `"sam"` occurs inside `"same"` and
/// `"wine"` inside `"winery"`, so a `forbid` list matched by substring fails
/// runs for no reason. This compares word by word, folding a regular plural,
/// because the corpus says *peanuts* while someone answering in one word says
/// *peanut* and means the same wrong thing.
pub fn says(text: &str, phrase: &str) -> bool {
    fn words(s: &str) -> Vec<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect()
    }
    fn same(a: &str, b: &str) -> bool {
        let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
        short == long
            || long.strip_suffix('s').is_some_and(|stem| stem == short)
            || long.strip_suffix("es").is_some_and(|stem| stem == short)
    }
    let (hay, needle) = (words(text), words(phrase));
    if needle.is_empty() || needle.len() > hay.len() {
        return false;
    }
    hay.windows(needle.len())
        .any(|w| w.iter().zip(&needle).all(|(a, b)| same(a, b)))
}

/// Whether any of `phrases` is said in `text`, and which.
pub fn says_any(text: &str, phrases: &[&str]) -> Option<String> {
    phrases
        .iter()
        .find(|p| says(text, p))
        .map(|p| (*p).to_string())
}

// ─── vocabulary ─────────────────────────────────────────────────────────────

const NAMES: &[&str] = &[
    "Nikhil", "Priya", "Devan", "Ananya", "Farhan", "Ishita", "Rohan", "Meera", "Arjun", "Sneha",
    "Kabir", "Tanvi", "Vikram", "Leela", "Aditya", "Nisha", "Ravi", "Divya", "Sameer", "Pooja",
    "Karthik", "Anjali", "Manish", "Shreya", "Gaurav", "Ritika", "Suresh", "Kavya", "Harsh",
    "Neha", "Omkar", "Lakshmi", "Jatin", "Radhika", "Varun", "Simran", "Yash", "Aarti", "Naveen",
    "Trisha",
];

const RELATIONS: &[&str] = &[
    "closest friend",
    "sister",
    "brother",
    "colleague",
    "manager",
    "neighbour",
    "cousin",
    "yoga teacher",
    "climbing partner",
    "accountant",
    "landlord",
    "book club host",
    "dentist",
    "mother-in-law",
    "former flatmate",
    "physiotherapist",
    "mentor",
    "squash partner",
    "travel agent",
    "team lead",
];

/// Drinks other people order. Cortado is deliberately absent.
const DRINKS: &[&str] = &[
    "cold brew",
    "flat white",
    "black filter coffee",
    "masala chai",
    "green tea",
    "americano",
    "cappuccino",
    "oat milk latte",
    "sparkling water",
    "ginger tea",
];

/// Allergens other people have. Sesame is deliberately absent.
const ALLERGENS: &[&str] = &[
    "peanuts",
    "shellfish",
    "dairy",
    "gluten",
    "soy",
    "kiwi",
    "walnuts",
    "mustard",
    "strawberries",
    "eggs",
];

/// Places other people favour. Fennelmark and Bellagrove are deliberately
/// absent — they belong to a needle and its trap.
const VENUES: &[&str] = &[
    "Harrowgate",
    "Silverbeam",
    "The Copper Lantern",
    "Marchetti",
    "Oldgrove Kitchen",
    "Ashvale",
    "The Blue Pergola",
    "Quillon",
    "Rosemarke",
    "The Salt Rope",
    "Verano",
    "Pinehold",
    "The Grey Heron",
    "Castellane",
    "Windermere",
    "Marlowe & Fig",
    "The Tallow Room",
    "Saffronbank",
    "Cloudberry",
    "The Iron Wren",
];

const VENUE_KINDS: &[&str] = &[
    "café", "bakery", "bar", "bookshop", "barber", "florist", "gym", "cinema",
];

const STREETS: &[&str] = &[
    "12th Main",
    "Alder Row",
    "Ferry Lane",
    "Kingsmill Street",
    "Orchard Walk",
    "Pike Street",
    "Quarry Road",
    "Sable Avenue",
    "Tanner's Yard",
    "Vine Hill",
];

const CITIES: &[&str] = &[
    "Chennai",
    "Mumbai",
    "Delhi",
    "Goa",
    "Pune",
    "Kochi",
    "Hyderabad",
    "Jaipur",
    "Kolkata",
    "Shillong",
    "Lisbon",
    "Berlin",
    "Kyoto",
    "Nairobi",
    "Reykjavik",
    "Bogotá",
];

const MONTHS: &[&str] = &[
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

const ARTISTS: &[&str] = &[
    "Sunfeather",
    "The Long Mile",
    "Kestrel Choir",
    "Amber Static",
    "Palegrove",
    "Norwood Trio",
    "Halcyon Drift",
    "The Paper Kites Cover Band",
    "Ostara",
    "Blue Cartography",
];

const ACTIVITIES: &[&str] = &[
    "cooking",
    "commuting",
    "running",
    "working late",
    "cleaning the flat",
    "walking the long way home",
    "packing for a trip",
    "doing the washing up",
];

const BRANDS: &[&str] = &[
    "Tarnhelm",
    "Windrose",
    "Castellane",
    "Idlewood",
    "Pinehold",
    "Saffronbank",
    "Cloudberry",
    "Verano",
];

const THINGS_OWNED: &[&str] = &[
    "headphones",
    "kettle",
    "camera",
    "backpack",
    "watch",
    "espresso machine",
    "turntable",
    "tent",
];

const PROJECTS: &[&str] = &[
    "the billing migration",
    "the winter catalogue",
    "the flat renovation",
    "the community garden rota",
    "the podcast relaunch",
    "the half marathon plan",
];

const ERRANDS: &[&str] = &[
    "return the library books",
    "renew the road tax",
    "book the boiler service",
    "send the birthday card",
    "collect the dry cleaning",
    "pick up the prescription",
];

const GIFTS: &[&str] = &[
    "a wool blanket",
    "a set of chef's knives",
    "a bird feeder",
    "a leather notebook",
    "a pair of walking boots",
    "a film camera",
];

/// A tiny deterministic mixer, so generated records vary without `rand` and
/// without a run-to-run difference that would make a failure unreproducible.
fn mix(seed: usize, salt: usize) -> usize {
    let mut x =
        seed.wrapping_mul(6_364_136_223_846_793_005) ^ salt.wrapping_mul(1_442_695_040_888_963_407);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 29;
    x
}

fn pick<'a>(table: &[&'a str], seed: usize, salt: usize) -> &'a str {
    table[mix(seed, salt) % table.len()]
}

// ─── record construction ────────────────────────────────────────────────────

/// Build one record.
///
/// Every field a scorer reads is filled deliberately: the statement is what the
/// model is shown, the tags and aliases are what a question has to match, and
/// the subject is what tells a needle from its trap.
#[allow(clippy::too_many_arguments)]
fn record(
    owner: &UserId,
    id: &str,
    kind: MemoryKind,
    predicate: &str,
    subject: EntityRef,
    statement: &str,
    tags: &[&str],
    aliases: &[&str],
) -> CanonicalMemory {
    let now = Utc::now();
    let subject_form = normalize_token(&subject.display);
    let entities = subject.surface_forms();
    CanonicalMemory {
        id: MemoryId::new(id),
        owner: owner.clone(),
        kind,
        predicate: CanonicalPredicate::new(predicate),
        status: MemoryStatus::Active,
        confidence: 0.9,
        subject,
        value: MemoryValue::Text(statement.to_string()),
        statement: statement.to_string(),
        evidence_summary: "Stated by the user in an earlier conversation.".into(),
        source: MemorySource::from_explicitness(
            Explicitness::ExplicitStatement,
            SessionId::new("ses_history"),
            TurnId(1),
        ),
        temporal: TemporalMetadata::created_at(now),
        retrieval: RetrievalMetadata {
            subject: subject_form,
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            aliases: aliases.iter().map(|a| (*a).to_string()).collect(),
            entities,
            location: None,
        },
        evidence: EvidenceCounters {
            count: 2,
            distinct_sessions: 2,
            distinct_days: 2,
        },
        privacy: PrivacyMetadata::default(),
        temporal_scope: TemporalScope::Persistent,
        supersedes: Vec::new(),
        superseded_by: None,
        qualifier: None,
    }
}

/// Mark a record as a recent event rather than a durable fact.
fn as_episode(mut memory: CanonicalMemory, days_ago: i64) -> CanonicalMemory {
    memory.kind = MemoryKind::Episodic;
    memory.temporal_scope = TemporalScope::RecentHistory;
    memory.temporal.valid_from = Utc::now() - Duration::days(days_ago);
    memory.temporal.expires_at = Some(Utc::now() + Duration::days(45));
    memory
}

/// Rhea, the user's wife — the subject of two needles.
pub fn rhea() -> EntityRef {
    EntityRef::named("Rhea")
        .with_alias("my wife")
        .with_alias("wife")
        .with_alias("partner")
}

fn person(i: usize) -> EntityRef {
    let name = NAMES[i % NAMES.len()];
    let relation = RELATIONS[mix(i, 7) % RELATIONS.len()];
    EntityRef::named(name).with_alias(format!("my {relation}"))
}

/// The `i`th generated record: twelve shapes, cycled.
fn generated(owner: &UserId, i: usize) -> CanonicalMemory {
    let id = format!("mem_gen_{i:05}");
    let name = NAMES[i % NAMES.len()];
    match i % 13 {
        0 => record(
            owner,
            &id,
            MemoryKind::Relationship,
            "social_relation",
            person(i),
            &format!(
                "{name} is the user's {}.",
                RELATIONS[mix(i, 7) % RELATIONS.len()]
            ),
            &["people", "relationship", "family", "friend"],
            &[],
        ),
        1 => record(
            owner,
            &id,
            MemoryKind::RelationshipPreference,
            "beverage_preference",
            person(i),
            &format!("{name} drinks {}.", pick(DRINKS, i, 11)),
            &["coffee", "drink", "beverage", "order", "café"],
            &[&format!("{name}'s usual order")],
        ),
        2 => record(
            owner,
            &id,
            MemoryKind::RelationshipPreference,
            "food_allergy",
            person(i),
            &format!("{name} is allergic to {}.", pick(ALLERGENS, i, 13)),
            &["allergy", "allergic", "food", "cannot eat"],
            &[],
        ),
        3 => record(
            owner,
            &id,
            MemoryKind::RelationshipPreference,
            "venue_preference",
            person(i),
            &format!("{name}'s favourite restaurant is {}.", pick(VENUES, i, 17)),
            &["restaurant", "venue", "dinner", "favourite", "eat"],
            &[&format!("where {name} likes to eat")],
        ),
        4 => record(
            owner,
            &id,
            MemoryKind::RelationshipPreference,
            "birthday",
            person(i),
            &format!("{name}'s birthday is in {}.", pick(MONTHS, i, 19)),
            &["birthday", "date", "celebration", "people"],
            &[],
        ),
        5 => {
            let kind_of_place = pick(VENUE_KINDS, i, 23);
            record(
                owner,
                &id,
                MemoryKind::LocationPreference,
                "local_place",
                EntityRef::user(),
                &format!(
                    "The user's usual {kind_of_place} is {} on {}.",
                    pick(VENUES, i, 29),
                    pick(STREETS, i, 31)
                ),
                &["place", "local", "usual", kind_of_place],
                &[],
            )
        }
        6 => record(
            owner,
            &id,
            MemoryKind::Routine,
            "weekly_routine",
            EntityRef::user(),
            &format!(
                "The user goes {} on {}s.",
                pick(ACTIVITIES, i, 37),
                [
                    "Monday",
                    "Tuesday",
                    "Wednesday",
                    "Thursday",
                    "Friday",
                    "Saturday",
                    "Sunday"
                ][mix(i, 41) % 7]
            ),
            &["routine", "habit", "weekly", "schedule"],
            &[],
        ),
        7 => record(
            owner,
            &id,
            MemoryKind::Preference,
            "music_preference",
            EntityRef::user(),
            &format!(
                "The user listens to {} while {}.",
                pick(ARTISTS, i, 43),
                pick(ACTIVITIES, i, 47)
            ),
            &["music", "listening", "audio", "preference"],
            &[],
        ),
        8 => record(
            owner,
            &id,
            MemoryKind::Preference,
            "possession",
            EntityRef::user(),
            &format!(
                "The user's {} is a {}.",
                pick(THINGS_OWNED, i, 53),
                pick(BRANDS, i, 59)
            ),
            &["owns", "possession", "device", "kit"],
            &[],
        ),
        9 => as_episode(
            record(
                owner,
                &id,
                MemoryKind::Episodic,
                "recent_event",
                EntityRef::user(),
                &format!(
                    "The user visited {} with {name} in {}.",
                    pick(CITIES, i, 61),
                    pick(MONTHS, i, 67)
                ),
                &["trip", "travel", "visited", "recent"],
                &[],
            ),
            (mix(i, 71) % 40) as i64 + 1,
        ),
        10 => {
            let mut commitment = record(
                owner,
                &id,
                MemoryKind::Commitment,
                "errand",
                EntityRef::user(),
                &format!(
                    "The user agreed to {} before {}.",
                    pick(ERRANDS, i, 73),
                    pick(MONTHS, i, 79)
                ),
                &["errand", "promise", "commitment", "todo"],
                &[],
            );
            commitment.temporal_scope = TemporalScope::Scheduled;
            commitment
        }
        11 => record(
            owner,
            &id,
            MemoryKind::RelationshipPreference,
            "gift_idea",
            person(i),
            &format!("{name} has been wanting {}.", pick(GIFTS, i, 89)),
            &["gift", "present", "birthday", "wishlist", "wants"],
            &[&format!("what to get {name}")],
        ),
        _ => record(
            owner,
            &id,
            MemoryKind::Project,
            "project",
            EntityRef::user(),
            &format!(
                "The user is working on {} with {name}.",
                pick(PROJECTS, i, 83)
            ),
            &["work", "project", "ongoing"],
            &[],
        ),
    }
}

/// The needles, and the traps set for them.
///
/// Appended after the generated bulk so their ids are stable at any size, and
/// so the decoy mass around them grows with the corpus rather than staying
/// fixed.
#[allow(clippy::vec_init_then_push)]
fn needles(owner: &UserId) -> Vec<CanonicalMemory> {
    let user = EntityRef::user;
    let mut out = Vec::new();

    // ── the user's usual order, against everyone else's ──
    out.push(record(
        owner,
        "mem_user_coffee",
        MemoryKind::Preference,
        "beverage_preference",
        user(),
        "The user's usual coffee order is a cortado.",
        &["coffee", "drink", "beverage", "order", "café"],
        &["my usual order", "my coffee"],
    ));
    out.push(as_episode(
        record(
            owner,
            "mem_trap_coldbrew",
            MemoryKind::Episodic,
            "recent_event",
            user(),
            "The user bought a box of cold brew cans for the office fridge.",
            &["coffee", "drink", "beverage", "order", "office"],
            &["the office coffee"],
        ),
        9,
    ));

    // ── an allergy: the fact a consumer assistant must never get wrong ──
    out.push(record(
        owner,
        "mem_user_allergy",
        MemoryKind::Identity,
        "food_allergy",
        user(),
        "The user is allergic to sesame.",
        &["allergy", "allergic", "food", "cannot eat"],
        &["my allergy", "what I cannot eat"],
    ));
    out.push(as_episode(
        record(
            owner,
            "mem_trap_peanut",
            MemoryKind::Episodic,
            "recent_event",
            user(),
            "A guest had an allergic reaction to peanuts at a dinner the user hosted.",
            &["allergy", "allergic", "reaction", "food", "peanuts"],
            &["the dinner where someone reacted"],
        ),
        21,
    ));

    // ── the wife's favourite place, against the user's own ──
    out.push(record(
        owner,
        "mem_rhea",
        MemoryKind::Relationship,
        "spouse",
        rhea(),
        "Rhea is the user's wife.",
        &["family", "wife", "spouse", "married"],
        &["my wife"],
    ));
    out.push(record(
        owner,
        "mem_rhea_restaurant",
        MemoryKind::RelationshipPreference,
        "venue_preference",
        rhea(),
        "Rhea's favourite restaurant is Fennelmark.",
        &["restaurant", "venue", "dinner", "favourite", "eat"],
        &["where Rhea likes to eat", "my wife's favourite restaurant"],
    ));
    out.push(record(
        owner,
        "mem_user_restaurant",
        MemoryKind::Preference,
        "venue_preference",
        user(),
        "The user's favourite restaurant is Bellagrove.",
        &["restaurant", "venue", "dinner", "favourite", "eat"],
        &["where I like to eat"],
    ));

    // ── a gift the assistant is expected to remember ──
    out.push(record(
        owner,
        "mem_rhea_gift",
        MemoryKind::RelationshipPreference,
        "gift_idea",
        rhea(),
        "Rhea has been hinting at the Skylark record player for her birthday.",
        &["gift", "present", "birthday", "wishlist", "wants"],
        &["what to get Rhea", "Rhea's birthday present"],
    ));
    out.push(record(
        owner,
        "mem_trap_wishlist",
        MemoryKind::Preference,
        "gift_idea",
        user(),
        "The user has a Tarnhelm espresso machine on their own wishlist.",
        &["gift", "present", "wishlist", "wants"],
        &["what I want"],
    ));

    // ── one of three venues the user belongs to ──
    out.push(record(
        owner,
        "mem_user_gym",
        MemoryKind::Preference,
        "club_membership",
        user(),
        "The user climbs at Karvala Boulders.",
        &["climbing", "gym", "club", "membership", "sport"],
        &["my climbing gym"],
    ));
    out.push(record(
        owner,
        "mem_trap_gym",
        MemoryKind::Preference,
        "club_membership",
        user(),
        "The user's weights gym is Ridgeline Fitness.",
        &["gym", "club", "membership", "fitness", "sport"],
        &["my gym"],
    ));
    out.push(record(
        owner,
        "mem_trap_sailing",
        MemoryKind::RelationshipPreference,
        "club_membership",
        EntityRef::named("Devan").with_alias("my brother"),
        "Devan sails at Mirador Yacht Club.",
        &["sailing", "club", "membership", "sport"],
        &["Devan's club"],
    ));

    // ── a correction, with the old value still on disk ──
    out.push(record(
        owner,
        "mem_barber_now",
        MemoryKind::Preference,
        "barber",
        user(),
        "The user's barber is Deepa at Tuloma Salon.",
        &["barber", "haircut", "hair", "salon"],
        &["who cuts my hair", "my barber"],
    ));
    let mut old_barber = record(
        owner,
        "mem_barber_old",
        MemoryKind::Preference,
        "barber",
        user(),
        "The user's barber is Girish at Tuloma Salon.",
        &["barber", "haircut", "hair", "salon"],
        &["who cuts my hair", "my barber"],
    );
    old_barber.status = MemoryStatus::Superseded;
    old_barber.superseded_by = Some(MemoryId::new("mem_barber_now"));
    out.push(old_barber);
    for (i, (who, whose)) in [("Rhea", "Anisha"), ("Priya", "Kabini")].iter().enumerate() {
        out.push(record(
            owner,
            &format!("mem_barber_other_{i}"),
            MemoryKind::RelationshipPreference,
            "barber",
            EntityRef::named(*who),
            &format!("{who}'s hairdresser is {whose}."),
            &["barber", "haircut", "hair", "salon", "hairdresser"],
            &[&format!("{who}'s salon")],
        ));
    }

    // ── an errand, against someone else running the same one elsewhere ──
    let mut cake = record(
        owner,
        "mem_commitment_cake",
        MemoryKind::Commitment,
        "errand",
        user(),
        "The user promised to collect Priya's cake from the Ashgrove bakery on Saturday.",
        &["errand", "promise", "collect", "bakery", "priya"],
        &["what I am collecting for Priya"],
    );
    cake.temporal_scope = TemporalScope::Scheduled;
    // The record is *about* the user but *mentions* Priya, and the entities
    // field is what carries that, at the same weight as a subject. Without it
    // every record whose subject is Priya outranks this one on her name alone.
    cake.retrieval.entities.push("priya".into());
    out.push(cake);
    let mut rival = record(
        owner,
        "mem_trap_windrose",
        MemoryKind::RelationshipPreference,
        "errand",
        EntityRef::named("Devan").with_alias("my brother"),
        "Devan is collecting his order from the Windrose bakery this week.",
        &["errand", "collect", "bakery", "devan"],
        &["what Devan is collecting"],
    );
    rival.temporal_scope = TemporalScope::Scheduled;
    out.push(rival);
    out.push(record(
        owner,
        "mem_usual_bakery",
        MemoryKind::LocationPreference,
        "local_place",
        user(),
        "The user's usual bakery is Marlowe & Fig on Vine Hill.",
        &["bakery", "place", "local", "usual"],
        &["where I usually go"],
    ));

    // ── a possession, against two the user merely discussed ──
    out.push(record(
        owner,
        "mem_user_bike",
        MemoryKind::Preference,
        "possession",
        user(),
        "The user rides a Thornbury bicycle.",
        &["bike", "bicycle", "owns", "possession", "cycling"],
        &["my bike"],
    ));
    out.push(as_episode(
        record(
            owner,
            "mem_trap_bike_test",
            MemoryKind::Episodic,
            "recent_event",
            user(),
            "The user test-rode a Castellane bicycle and found it too heavy.",
            &["bike", "bicycle", "cycling", "test", "shop"],
            &["the bike I tried"],
        ),
        16,
    ));
    out.push(record(
        owner,
        "mem_trap_bike_devan",
        MemoryKind::RelationshipPreference,
        "possession",
        EntityRef::named("Devan").with_alias("my brother"),
        "Devan rides an Idlewood bicycle.",
        &["bike", "bicycle", "owns", "possession", "cycling"],
        &["Devan's bike"],
    ));

    out
}

/// Generate a corpus of roughly `size` records, needles included.
///
/// Deterministic: the same `size` always produces the same corpus, so a failure
/// at 1,200 records can be reproduced exactly.
pub fn generate(owner: &UserId, size: usize) -> Vec<CanonicalMemory> {
    let extras = needles(owner);
    let bulk = size.saturating_sub(extras.len());
    let mut out: Vec<CanonicalMemory> = (0..bulk).map(|i| generated(owner, i)).collect();
    out.extend(extras);
    out
}

/// The default-size corpus.
pub fn corpus(owner: &UserId) -> Vec<CanonicalMemory> {
    generate(owner, DEFAULT_SIZE)
}

// ─── installation ───────────────────────────────────────────────────────────

/// Write records into the engine's repository and compile the index.
///
/// Records go in through the real transactional commit, so they are rendered to
/// OKF Markdown on disk and read back through the parser before any test asks a
/// question of them. Seeding the object graph directly would skip the
/// serialization round trip that a returning user's memory depends on.
pub async fn install_records(
    engine: &MemoryEngine,
    records: Vec<CanonicalMemory>,
) -> Vec<CanonicalMemory> {
    let mut transaction = MemoryTransaction::new(engine.user().clone(), "corpus-seed");
    for record in &records {
        transaction = transaction.put(record.clone());
    }
    engine
        .repository()
        .commit(transaction)
        .await
        .expect("seeding the corpus must commit");
    engine
        .compile_index()
        .await
        .expect("the index must compile");
    records
}

/// Install the default-size corpus.
pub async fn install(engine: &MemoryEngine) -> Vec<CanonicalMemory> {
    install_records(engine, corpus(engine.user())).await
}

/// Build one arbitrary active record, for a test that needs its own corpus.
#[allow(clippy::too_many_arguments)]
pub fn make(
    owner: &UserId,
    id: &str,
    kind: MemoryKind,
    predicate: &str,
    subject: EntityRef,
    statement: &str,
    tags: &[&str],
    aliases: &[&str],
) -> CanonicalMemory {
    record(
        owner, id, kind, predicate, subject, statement, tags, aliases,
    )
}

/// Every record in the namespace, read back from the repository.
pub async fn installed(engine: &MemoryEngine) -> Vec<CanonicalMemory> {
    engine
        .repository()
        .all(engine.user())
        .await
        .expect("reading the corpus back")
}

/// Look one record up by id.
pub fn by_id<'a>(records: &'a [CanonicalMemory], id: &str) -> Option<&'a CanonicalMemory> {
    records.iter().find(|m| m.id.as_str() == id)
}

/// The statements a `recall_context` payload carried back, lowercased.
pub fn payload_statements(payload: &serde_json::Value) -> Vec<String> {
    payload["facts"]
        .as_array()
        .map(|facts| {
            facts
                .iter()
                .filter_map(|f| f["statement"].as_str())
                .map(str::to_lowercase)
                .collect()
        })
        .unwrap_or_default()
}
