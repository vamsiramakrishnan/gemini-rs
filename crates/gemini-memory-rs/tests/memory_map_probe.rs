//! Does telling the model what is *in* memory make it write better filters?
//!
//! # The idea
//!
//! `filter_dsl_probe` shows that a correct `about`+`kind` filter takes fused
//! top-5 from 79 of 93 to 90, that a wrong one applied softly costs two, and
//! that the break-even accuracy is 15%. What it could not say is how accurate a
//! real model would be, because the filters there were derived from the answer.
//!
//! The obvious way to make a model accurate about a closed vocabulary is to
//! show it the vocabulary. Give it a compressed inventory of the corpus — which
//! subjects exist, which predicates, which kinds, with counts — the way a
//! coding agent is given a map of the repository before being asked to name a
//! file. It stops guessing that the user's haircut is filed under `hairdresser`
//! and picks `barber` off a list.
//!
//! That last example is not decoration. `a_subject_less_recall_does_not_default_to_the_user`
//! is `#[ignore]`d in this crate precisely because a query for "hairdresser"
//! cannot match a record that says "barber", and no ranking fixes what was
//! never retrieved. A map attacks that at the *query* end rather than the index
//! end, which is a cheaper place to fix it.
//!
//! # Where the map has to live
//!
//! Not in the tool schema. Live sessions freeze tool declarations at connect
//! time — the engine's own docs are explicit that instruction updates are
//! allowed mid-session and tool definitions are not — and the corpus grows
//! while the session runs. So the map belongs in the system instruction, which
//! *can* be updated, or in an injected context turn. That also makes it cheap:
//! it is set once and cached, not resent per call.
//!
//! # What is measured
//!
//! 1. **Does the map stay small?** Vocabulary size and token cost across
//!    250 → 16,000 records. If predicates grow with the corpus the idea does
//!    not survive contact with a real user's memory.
//! 2. **Does it make the model accurate?** The same 93 questions, put to
//!    `gemini-2.5-flash-lite` twice — once cold, once with the map — asked for
//!    `about`, `kind` and `attribute`. Scored against the record that actually
//!    answers the question.
//! 3. **What does that accuracy buy?** The measured accuracy fed through
//!    `filter_dsl_probe`'s break-even, to an expected fused top-5.
//!
//! # What it found
//!
//! **The map is bounded by the vocabulary, not the corpus.**
//!
//! | records | subjects | predicates | kinds | map tokens |
//! |---|---|---|---|---|
//! | 250 | 38 | 16 | 9 | 242 |
//! | 1,000 | 42 | 16 | 9 | 262 |
//! | 4,000 | 42 | 16 | 9 | 267 |
//! | 16,000 | 42 | 16 | 9 | **282** |
//!
//! Flat from a thousand records up: a person accumulates more facts about the
//! same handful of people and properties, not more kinds of thing. 282 tokens
//! against 16,000 records, set once in the system instruction and never resent.
//!
//! **It transforms the model's accuracy on the field that matters.**
//!
//! | condition | `about` | `kind` | `attribute` | `about`+`kind` | `about`+`attribute` |
//! |---|---|---|---|---|---|
//! | cold, no map | 51% | 4% | 2% | 3% | 2% |
//! | with memory map | 68% | 14% | **68%** | 14% | **49%** |
//!
//! `attribute` goes from 2% to 68% — thirty-four times better. That is the
//! vocabulary gap closing exactly as hoped: a model cannot guess that a
//! haircut is filed under `barber` or a coffee order under
//! `beverage_preference`, and it does not have to guess when the values are in
//! front of it.
//!
//! **`kind` stays broken, and that is the interesting part.** 4% to 14%; the
//! map barely helps. Every remaining failure is the same shape — the model says
//! `Preference` or `Episodic` where the corpus says `Identity`, because a food
//! allergy is filed as identity and a model reasonably calls it a preference.
//! `kind` is an internal classification decision, not a property of the
//! question, so showing the model the vocabulary does not tell it which bucket
//! *this* engine's extractor chose.
//!
//! The engine already knew this. `LocalMemoryRetriever::run_lexical` refuses to
//! apply a plan's scopes as a filter, and says why: *"making retrieval depend
//! on those two agreeing on a taxonomy loses recall for no benefit. A dietary
//! fact the extractor filed as `Identity` is exactly what a plan scoped to
//! `Preference` is looking for."* This is that comment, measured.
//!
//! **So the filter is `about` + `attribute`, and with the map it pays.**
//!
//! | condition | filter | accuracy | expected fused top-5 |
//! |---|---|---|---|
//! | cold | `about`+`kind` | 3% | 77.4/93 — worse than not filtering |
//! | cold | `about`+`attribute` | 2% | 78.3/93 — worse than not filtering |
//! | map | `about`+`kind` | 14% | 78.8/93 — worse than not filtering |
//! | **map** | **`about`+`attribute`** | **49%** | **84.4/93 — better** |
//!
//! Without the map, no filter is worth writing: the model is below break-even
//! on every combination, and the feature would be a net loss. With it, and on
//! the right pair of fields, it is worth five questions over the unfiltered
//! baseline of 79 — for 282 tokens and no extra round trip.
//!
//! Model answers are cached by content hash, so re-runs are free. Skips without
//! an API key.

#![cfg(feature = "gemini-llm")]

mod common;

use std::collections::{BTreeMap, HashMap};

use common::corpus::{self, PROBES};
use common::paraphrase;
use common::{file_backed_engine, have_api_key, skip, ScratchDir};

use gemini_memory_rs::core::{stable_hash, CanonicalMemory, MemoryStatus, UserId};

/// Cheapest model that writes a small structured object, and the one
/// `serving_latency_probe` found has the lower TTFT — which is what a
/// per-turn call is priced by.
const MODEL: &str = "gemini-2.5-flash-lite";

/// Corpus sizes the map is measured against.
const SIZES: &[usize] = &[250, 1_000, 4_000, 16_000];

/// How many entries of each kind the map carries before truncating.
const MAP_LIMIT: usize = 40;

/// Rough token estimate, matching the engine's own conservative rule.
fn tokens(text: &str) -> usize {
    text.split_whitespace()
        .count()
        .max(text.chars().count().div_ceil(4))
}

// ─── the map ────────────────────────────────────────────────────────────────

/// A compressed inventory of what memory holds.
///
/// Counts are included deliberately: they tell the model which values are worth
/// filtering by and which are one-offs, and they cost almost nothing.
fn build_map(records: &[&CanonicalMemory]) -> String {
    let mut subjects: BTreeMap<&str, usize> = BTreeMap::new();
    let mut predicates: BTreeMap<&str, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for memory in records {
        *subjects.entry(&memory.retrieval.subject).or_default() += 1;
        *predicates.entry(memory.predicate.as_str()).or_default() += 1;
        *kinds.entry(format!("{:?}", memory.kind)).or_default() += 1;
    }

    let render = |label: &str, counts: Vec<(String, usize)>| -> String {
        let mut sorted = counts;
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let shown: Vec<String> = sorted
            .iter()
            .take(MAP_LIMIT)
            .map(|(name, n)| format!("{name} ({n})"))
            .collect();
        let tail = sorted.len().saturating_sub(MAP_LIMIT);
        let suffix = if tail > 0 {
            format!(", and {tail} more")
        } else {
            String::new()
        };
        format!("{label}: {}{suffix}\n", shown.join(", "))
    };

    let mut map = String::from("MEMORY MAP — the values that exist in this user's memory.\n");
    map.push_str(&render(
        "about",
        subjects.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
    ));
    map.push_str(&render(
        "attribute",
        predicates
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect(),
    ));
    map.push_str(&render(
        "kind",
        kinds.iter().map(|(k, v)| (k.clone(), *v)).collect(),
    ));
    map
}

// ─── asking the model ───────────────────────────────────────────────────────

fn api_key() -> String {
    ["GEMINI_API_KEY", "GOOGLE_GENAI_API_KEY", "GOOGLE_API_KEY"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.trim().is_empty())
        .expect("an API key, checked by the caller")
}

fn prompt_for(question: &str, map: Option<&str>) -> String {
    let preamble = match map {
        Some(map) => format!("{map}\n"),
        None => String::new(),
    };
    format!(
        "{preamble}A user of a voice assistant said:\n\n  \"{question}\"\n\n\
         You are about to search their memory. Fill in the filter fields that \
         narrow the search, using ONLY values that exist{}.\n\n\
         Reply with JSON and nothing else:\n\
         {{\"about\": \"...\", \"kind\": \"...\", \"attribute\": \"...\"}}\n\n\
         `about` is whose fact it is. `kind` is what sort of memory. \
         `attribute` is which property of them. Use null for any field you are \
         not confident about.",
        if map.is_some() {
            " in the map above"
        } else {
            ""
        }
    )
}

async fn ask(client: &reqwest::Client, key: &str, prompt: &str) -> Option<serde_json::Value> {
    let url =
        format!("https://generativelanguage.googleapis.com/v1beta/models/{MODEL}:generateContent");
    let body = serde_json::json!({
        "contents": [{ "role": "user", "parts": [{ "text": prompt }] }],
        "generationConfig": {
            "temperature": 0.0,
            "maxOutputTokens": 2048,
            "responseMimeType": "application/json",
        },
    });
    let mut backoff = std::time::Duration::from_millis(500);
    for attempt in 0..5 {
        match client
            .post(&url)
            .header("x-goog-api-key", key)
            .json(&body)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                let json: serde_json::Value = response.json().await.ok()?;
                let text = json["candidates"][0]["content"]["parts"]
                    .as_array()?
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("");
                return serde_json::from_str(text.trim()).ok();
            }
            Ok(response) if attempt < 4 => {
                eprintln!("  filter call {} — retrying", response.status());
            }
            Ok(_) | Err(_) => {}
        }
        tokio::time::sleep(backoff).await;
        backoff *= 2;
    }
    None
}

fn field<'a>(value: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    value
        .get(name)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("null"))
}

#[derive(Default)]
struct Accuracy {
    asked: usize,
    about: usize,
    kind: usize,
    attribute: usize,
    about_and_kind: usize,
    about_and_attribute: usize,
    abstained: usize,
}

impl Accuracy {
    fn row(&self, label: &str) -> String {
        let pct = |n: usize| 100.0 * n as f64 / self.asked.max(1) as f64;
        let cell = |n: usize| format!("{:.0}%", pct(n));
        let (about, kind) = (cell(self.about), cell(self.kind));
        let attribute = cell(self.attribute);
        let and_kind = cell(self.about_and_kind);
        let and_attribute = cell(self.about_and_attribute);
        let abstained = format!("{}/{}", self.abstained, self.asked);
        format!(
            "{label:<22} {about:<9} {kind:<9} {attribute:<9} {and_kind:<14} \
             {and_attribute:<18} {abstained}\n"
        )
    }
}

#[tokio::test]
async fn does_a_memory_map_make_the_model_write_better_filters() {
    // ── 1. does the map stay small? ──
    let mut report = String::from(
        "\ndoes a memory map stay small as memory grows?\n\n\
         records  subjects  predicates  kinds  map tokens\n",
    );
    let owner = UserId::new("usr_map");
    let mut largest_map = String::new();
    for &size in SIZES {
        let generated = corpus::generate(&owner, size);
        let active: Vec<&CanonicalMemory> = generated
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .collect();
        let subjects: std::collections::HashSet<&str> = active
            .iter()
            .map(|m| m.retrieval.subject.as_str())
            .collect();
        let predicates: std::collections::HashSet<&str> =
            active.iter().map(|m| m.predicate.as_str()).collect();
        let kinds: std::collections::HashSet<String> =
            active.iter().map(|m| format!("{:?}", m.kind)).collect();
        let map = build_map(&active);
        report.push_str(&format!(
            "{size:<8} {:<9} {:<11} {:<6} {}\n",
            subjects.len(),
            predicates.len(),
            kinds.len(),
            tokens(&map),
        ));
        largest_map = map;
    }
    report.push_str(&format!(
        "\nThe map is bounded by the *vocabulary*, not the corpus: a user acquires more\n\
         facts about the same handful of people and properties, not more kinds of thing.\n\
         At the top of this range it is {} tokens, set once in the system instruction and\n\
         never resent — against {} records.\n",
        tokens(&largest_map),
        SIZES.last().unwrap(),
    ));

    if !have_api_key() {
        eprintln!("{report}");
        return skip("does_a_memory_map_make_the_model_write_better_filters (model half)");
    }

    // ── 2. does it make the model accurate? ──
    let scratch = ScratchDir::new("memory-map");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let records = corpus::installed(&engine).await;
    let active: Vec<&CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();
    let map = build_map(&active);

    let cache_path =
        std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("memory-map-filters.json");
    let mut cache: HashMap<String, serde_json::Value> = std::fs::read_to_string(&cache_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let client = reqwest::Client::new();
    let key = api_key();

    let mut cold = Accuracy::default();
    let mut mapped = Accuracy::default();
    let mut calls = 0usize;
    let mut examples: Vec<String> = Vec::new();

    for (probe_name, phrasing) in paraphrase::all() {
        let probe = PROBES.iter().find(|p| p.name == probe_name).expect("probe");
        let truth = active
            .iter()
            .find(|m| m.id.as_str() == probe.target)
            .expect("target in corpus");

        for (with_map, tally) in [(false, &mut cold), (true, &mut mapped)] {
            let prompt = prompt_for(phrasing.query, with_map.then_some(map.as_str()));
            let cache_key = stable_hash(&format!("{MODEL}|{prompt}"));
            let answer = match cache.get(&cache_key) {
                Some(cached) => cached.clone(),
                None => {
                    let Some(fresh) = ask(&client, &key, &prompt).await else {
                        continue;
                    };
                    cache.insert(cache_key, fresh.clone());
                    calls += 1;
                    if calls.is_multiple_of(20) {
                        let _ = std::fs::write(
                            &cache_path,
                            serde_json::to_string(&cache).unwrap_or_default(),
                        );
                    }
                    fresh
                }
            };

            let said_about = field(&answer, "about");
            let said_kind = field(&answer, "kind");
            let said_attribute = field(&answer, "attribute");

            let about_ok =
                said_about.is_some_and(|v| v.eq_ignore_ascii_case(&truth.retrieval.subject));
            let kind_ok =
                said_kind.is_some_and(|v| v.eq_ignore_ascii_case(&format!("{:?}", truth.kind)));
            let attribute_ok =
                said_attribute.is_some_and(|v| v.eq_ignore_ascii_case(truth.predicate.as_str()));

            tally.asked += 1;
            tally.about += usize::from(about_ok);
            tally.kind += usize::from(kind_ok);
            tally.attribute += usize::from(attribute_ok);
            tally.about_and_kind += usize::from(about_ok && kind_ok);
            tally.about_and_attribute += usize::from(about_ok && attribute_ok);
            if said_about.is_none() && said_kind.is_none() && said_attribute.is_none() {
                tally.abstained += 1;
            }

            if with_map && examples.len() < 6 && !(about_ok && kind_ok) {
                examples.push(format!(
                    "  {:?}\n    said  about={:?} kind={:?} attribute={:?}\n    truth about={:?} kind={:?} attribute={:?}",
                    phrasing.query,
                    said_about.unwrap_or("—"),
                    said_kind.unwrap_or("—"),
                    said_attribute.unwrap_or("—"),
                    truth.retrieval.subject,
                    format!("{:?}", truth.kind),
                    truth.predicate.as_str(),
                ));
            }
        }
    }
    let _ = std::fs::write(
        &cache_path,
        serde_json::to_string(&cache).unwrap_or_default(),
    );

    report.push_str(&format!(
        "\n\nhow accurately {MODEL} fills the filter, over {} questions ({calls} calls this run)\n\n\
         {:<22} {:<9} {:<9} {:<9} {:<14} {:<18} {}\n",
        cold.asked,
        "condition",
        "about",
        "kind",
        "attribute",
        "about + kind",
        "about + attribute",
        "abstained",
    ));
    report.push_str(&cold.row("cold (no map)"));
    report.push_str(&mapped.row("with memory map"));

    // ── 3. what does that accuracy buy? ──
    //
    // From `filter_dsl_probe`, over the same corpus and question set: fused
    // top-5 is 79/93 unfiltered, 90/93 with a correct soft `about`+`kind`
    // filter, and 77/93 with a wrong one.
    const BASELINE: f64 = 79.0;
    report.push_str(&format!(
        "\nwhat that buys, through `filter_dsl_probe`'s soft-fusion numbers\n\
         (unfiltered 79/93; about+kind 90 right / 77 wrong; about+attribute 91 / 78)\n\n\
         {:<22} {:<20} {:<12} {}\n",
        "condition", "filter", "accuracy", "expected fused top-5"
    ));
    for (label, tally) in [("cold (no map)", &cold), ("with memory map", &mapped)] {
        for (filter, hits, right, wrong) in [
            ("about + kind", tally.about_and_kind, 90.0, 77.0),
            ("about + attribute", tally.about_and_attribute, 91.0, 78.0),
        ] {
            let p = hits as f64 / tally.asked.max(1) as f64;
            let expected = p * right + (1.0 - p) * wrong;
            report.push_str(&format!(
                "{label:<22} {filter:<20} {:<12} {expected:.1}/93  {}\n",
                format!("{:.0}%", p * 100.0),
                if expected > BASELINE {
                    "BETTER than not filtering"
                } else {
                    "worse than not filtering"
                }
            ));
        }
    }

    if !examples.is_empty() {
        report.push_str("\nwhere it still gets `about`+`kind` wrong, with the map:\n");
        for example in &examples {
            report.push_str(&format!("{example}\n"));
        }
    }
    eprintln!("{report}");

    assert!(
        mapped.asked > 0,
        "no filter was obtained from the model at all\n{report}"
    );

    // The map must stay a fixed cost. If the vocabulary ever grows with the
    // corpus, it stops being something you can put in a system instruction.
    assert!(
        tokens(&largest_map) < 800,
        "the map is {} tokens at {} records. It is meant to be bounded by the \
         vocabulary rather than the corpus; at this size it belongs in retrieval \
         rather than in the instruction.\n{report}",
        tokens(&largest_map),
        SIZES.last().unwrap(),
    );

    // The finding the recommendation rests on: with the map, `about`+`attribute`
    // clears its 8% break-even by a wide margin, and `kind` does not clear its
    // 15% one at all. If a future model changes either, the recommendation
    // changes with it.
    let attribute_accuracy = mapped.about_and_attribute as f64 / mapped.asked.max(1) as f64;
    assert!(
        attribute_accuracy > 0.08,
        "with the map, `about`+`attribute` is {:.0}% accurate against a break-even of \
         8% — below that the filter costs more than it earns and should not be \
         built\n{report}",
        attribute_accuracy * 100.0,
    );
    assert!(
        mapped.about_and_attribute > mapped.about_and_kind,
        "`about`+`kind` ({}) has caught up with `about`+`attribute` ({}). The \
         recommendation to filter on the predicate rather than the kind rests on \
         the gap between them.\n{report}",
        mapped.about_and_kind,
        mapped.about_and_attribute,
    );
}
