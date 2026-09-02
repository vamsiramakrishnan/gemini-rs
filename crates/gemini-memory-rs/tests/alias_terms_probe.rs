//! Does the alias field earn its place, and what should be written into it?
//!
//! # The hypothesis
//!
//! Two earlier results point at each other.
//!
//! `semantic_fusion_probe` found that enriching each record with six
//! LLM-written *questions* is worse than doing nothing: 31 of 93 against 41 for
//! the bare statement. The diagnosis was dilution — six sentences run six times
//! the length of the statement, most of them generic, and they collide across a
//! thousand records.
//!
//! `query_rewrite_probe` found the opposite on the query side: expanding the
//! *query* with synonyms takes BM25 from 58 of 93 to 76. Synonyms are precisely
//! what an inverted index cannot generate for itself, and supplying them is
//! worth eighteen questions.
//!
//! Those are consistent if the thing that matters is not *whether* you expand
//! but *what shape* the expansion takes. Sentences dilute; terms do not. So the
//! hypothesis is that short noun phrases — four words, not four clauses —
//! written into `retrieval.aliases` at ingestion should buy the lexical gain
//! that query expansion buys, without the semantic loss that generated
//! sentences caused.
//!
//! If it holds it is the better place to spend the model call. Query expansion
//! costs one round trip *per query*, on the path where the user is waiting.
//! Alias terms cost one call *per record*, once, off the response path
//! entirely.
//!
//! # Why this is a question about the data structure
//!
//! `retrieval.aliases` already exists and BM25 already weights it 2.5, second
//! only to subject and entities. Nothing needs to be added to the schema; the
//! question is whether the field is worth filling and with what. That is the
//! cheapest kind of change — and it reaches all three retrieval modes at once,
//! since the aliases sit in the OKF Markdown frontmatter where `ripgrep` finds
//! them, in the inverted index where BM25 weights them, and in the embedded
//! text where the vector search sees them.
//!
//! # The three conditions
//!
//! | aliases | who wrote them |
//! |---|---|
//! | `none` | the field is empty — what the alias field is worth at all |
//! | `curated` | the fixture's own, which **saw the query set** — an upper bound |
//! | `generated` | short terms an LLM wrote from the statement alone |
//!
//! `generated` is the honest one, and the only one available in production: the
//! model is shown one statement and nothing else — no corpus, no probes, no
//! line of [`common::paraphrase`].
//!
//! # What it found: the hypothesis is wrong
//!
//! | aliases | lexical | semantic | fused 2:1 | chars added |
//! |---|---|---|---|---|
//! | none | 51/93 (0.448) | 73/93 (0.748) | **74/93** (0.699) | 0 |
//! | curated (fixture) | 58/93 (0.536) | 75/93 (0.720) | 74/93 (0.705) | 5 |
//! | **generated terms** | **45/93** (0.340) | **68/93** (0.663) | **63/93** (0.574) | 89 |
//!
//! Generated alias terms are worse than leaving the field **empty**, on every
//! retriever: six questions worse lexically, five worse semantically, eleven
//! worse fused. Short noun phrases did not avoid the failure that generated
//! sentences hit. They made it worse.
//!
//! # And not for the reason the hypothesis assumed
//!
//! The obvious explanation on reading a sample — *"Hairstylist Tuloma, Salon
//! Deepa, Barber Tuloma, Deepa hairstylist"* for a record that already says
//! Deepa, Tuloma, Salon and barber — is that the model recombined words the
//! statement already had. Measured, that is not it:
//!
//! | aliases | tokens that are new | records carrying any |
//! |---|---|---|
//! | curated | 72% | **24%** |
//! | generated | 74% | **100%** |
//!
//! The vocabulary is genuinely novel in both — 74% against 72%, indistinguishable.
//! What differs is **coverage**, and that is the whole result.
//!
//! Curated aliases sit on a quarter of the corpus. Generated ones sit on all of
//! it. Adding "hairstylist" to one record makes it a rare, discriminating term.
//! Adding a synonym set to all 1,199 records means every new term appears in
//! many of them, its IDF collapses, and it discriminates nothing — while still
//! lengthening a length-normalised field on every record. Uniform enrichment
//! spends precision on the overwhelming majority of records nobody was ever
//! going to ask about, to buy recall on the few they were.
//!
//! That reframes the earlier finding too. `semantic_fusion_probe` blamed
//! generated *sentences* on dilution — six times the length of the statement.
//! But those were also applied to 100% of records, and this result says the
//! shape was never the variable. Sentences versus terms was the wrong axis.
//! **Selective versus uniform** is the axis.
//!
//! # What that means for the data structure
//!
//! The alias field earns its place — curated aliases are worth seven lexical
//! questions — but it cannot be filled by a model sweeping the corpus, because
//! the value is in *not* filling most of it. Which turns a data-structure
//! question into a targeting question: which records deserve enrichment? At
//! ingestion, nothing knows. What knows is the query log — the questions that
//! came back thin, and the records a user actually reached for. Enriching those
//! is a different feature from the one tested here, and this file is the reason
//! to build that one instead.
//!
//! One caveat on the `curated` column: those aliases were written by the same
//! hand as the questions and sit disproportionately on the probe targets, so
//! its 58 is an upper bound rather than what careful authorship would generally
//! achieve. The comparison that carries the finding is `generated` against
//! `none`, and both of those are clean.
//!
//! Everything is cached by content hash. Skips without an API key.

#![cfg(feature = "gemini-llm")]

mod common;

use std::collections::HashMap;

use common::corpus::{self, PROBES};
use common::paraphrase;
use common::rank::{CANDIDATES, as_hits, fuse, lexical, rank_of};
use common::views::structural_view;
use common::{ScratchDir, file_backed_engine, have_api_key, skip};

use gemini_memory_rs::bm25::{IndexedMemory, MemoryIndex};
use gemini_memory_rs::core::{CanonicalMemory, MemoryId, MemoryStatus, stable_hash};

const EMBEDDING_MODEL: &str = "gemini-embedding-2";
const WIDTH: usize = 768;
const MODEL: &str = "gemini-2.5-flash-lite";
const TERMS_CACHE: &str = "alias-terms-cache.json";
const EMBED_CACHE: &str = "alias-terms-embeddings.json";
const CONCURRENCY: usize = 12;

/// The instruction that produces the `generated` condition.
///
/// Every clause of it is doing work. "Noun phrases" and "one to four words"
/// are what keep this from becoming the sentence enrichment that already
/// failed. "Words the fact does not already use" is the point of the exercise —
/// terms the statement already contains are terms BM25 already has.
const TERM_PROMPT: &str = "Here is one fact remembered about a person:\n\n\
     {statement}\n\n\
     Write five short search terms someone might use to find this fact. Noun \
     phrases only, one to four words each — not sentences, not questions. \
     Prefer words the fact does not already use. One per line, nothing else.";

fn api_key() -> String {
    ["GEMINI_API_KEY", "GOOGLE_GENAI_API_KEY", "GOOGLE_API_KEY"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.trim().is_empty())
        .expect("an API key, checked by the caller")
}

fn cache_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn load<T: serde::de::DeserializeOwned + Default>(name: &str) -> T {
    std::fs::read_to_string(cache_path(name))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save<T: serde::Serialize>(name: &str, value: &T) {
    if let Ok(raw) = serde_json::to_string(value) {
        let _ = std::fs::write(cache_path(name), raw);
    }
}

async fn post(
    client: &reqwest::Client,
    key: &str,
    url: &str,
    body: &serde_json::Value,
) -> Option<serde_json::Value> {
    let mut backoff = std::time::Duration::from_millis(500);
    for _ in 0..5 {
        if let Ok(response) = client
            .post(url)
            .header("x-goog-api-key", key)
            .json(body)
            .send()
            .await
            && response.status().is_success()
        {
            return response.json().await.ok();
        }
        tokio::time::sleep(backoff).await;
        backoff *= 2;
    }
    None
}

/// Generate the alias terms for one statement. Sampling left at the default.
async fn generate_terms(
    client: &reqwest::Client,
    key: &str,
    statement: &str,
) -> Option<Vec<String>> {
    let url =
        format!("https://generativelanguage.googleapis.com/v1beta/models/{MODEL}:generateContent");
    let body = serde_json::json!({
        "contents": [{ "role": "user", "parts": [{
            "text": TERM_PROMPT.replace("{statement}", statement)
        }]}],
        "generationConfig": { "maxOutputTokens": 2048 },
    });
    let json = post(client, key, &url, &body).await?;
    let text = json["candidates"][0]["content"]["parts"]
        .as_array()?
        .iter()
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    let terms: Vec<String> = text
        .lines()
        .map(|line| {
            line.trim()
                .trim_start_matches(['-', '*', '•'])
                .trim()
                .trim_matches('"')
                .to_string()
        })
        // Guard the shape the hypothesis depends on: anything long enough to be
        // a sentence is not the thing being tested.
        .filter(|line| !line.is_empty() && line.split_whitespace().count() <= 5)
        .take(6)
        .collect();
    (!terms.is_empty()).then_some(terms)
}

async fn embed(client: &reqwest::Client, key: &str, text: &str) -> Option<Vec<f32>> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{EMBEDDING_MODEL}:embedContent"
    );
    let body = serde_json::json!({
        "model": format!("models/{EMBEDDING_MODEL}"),
        "content": { "parts": [{ "text": text }] },
        "taskType": "RETRIEVAL_DOCUMENT",
        "outputDimensionality": WIDTH,
    });
    let json = post(client, key, &url, &body).await?;
    let values = json["embedding"]["values"].as_array()?;
    let mut vector: Vec<f32> = values
        .iter()
        .filter_map(serde_json::Value::as_f64)
        .map(|v| v as f32)
        .collect();
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    Some(vector)
}

fn embed_key(text: &str) -> String {
    stable_hash(&format!(
        "{EMBEDDING_MODEL}|{WIDTH}|RETRIEVAL_DOCUMENT|{text}"
    ))
}

/// The text a record is embedded as, under one alias condition.
fn view(memory: &CanonicalMemory, aliases: &[String]) -> String {
    if aliases.is_empty() {
        structural_view(memory)
    } else {
        format!(
            "{}\nAlso asked as: {}",
            structural_view(memory),
            aliases.join(", ")
        )
    }
}

fn semantic(query: &[f32], vectors: &[Vec<f32>]) -> Vec<(usize, f32)> {
    let mut scored: Vec<(usize, f32)> = vectors
        .iter()
        .enumerate()
        .map(|(i, v)| (i, v.iter().zip(query).map(|(a, b)| a * b).sum::<f32>()))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(CANDIDATES);
    scored
}

#[derive(Default, Clone, Copy)]
struct Tally {
    asked: usize,
    first: usize,
    top_five: usize,
    reciprocal: f64,
}

impl Tally {
    fn record(&mut self, rank: Option<usize>) {
        self.asked += 1;
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
            None => {}
        }
    }
    fn cell(&self) -> String {
        format!(
            "{}/{} ({:.3})",
            self.top_five,
            self.asked,
            self.reciprocal / self.asked.max(1) as f64
        )
    }
}

#[tokio::test]
async fn whether_short_alias_terms_beat_generated_sentences() {
    if !have_api_key() {
        return skip("whether_short_alias_terms_beat_generated_sentences");
    }

    let scratch = ScratchDir::new("alias-terms");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let records = corpus::installed(&engine).await;
    let active: Vec<CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .cloned()
        .collect();

    let client = reqwest::Client::new();
    let key = api_key();

    // ── generate the terms, one call per record, from the statement alone ──
    let mut terms: HashMap<String, Vec<String>> = load(TERMS_CACHE);
    let missing: Vec<String> = active
        .iter()
        .map(|m| m.statement.clone())
        .filter(|s| !terms.contains_key(s))
        .collect();
    if !missing.is_empty() {
        eprintln!("generating alias terms for {} records…", missing.len());
    }
    for chunk in missing.chunks(CONCURRENCY) {
        let mut set = tokio::task::JoinSet::new();
        for statement in chunk {
            let (client, key, statement) = (client.clone(), key.clone(), statement.clone());
            set.spawn(async move {
                let generated = generate_terms(&client, &key, &statement).await;
                (statement, generated)
            });
        }
        while let Some(joined) = set.join_next().await {
            if let Ok((statement, Some(generated))) = joined {
                terms.insert(statement, generated);
            }
        }
        save(TERMS_CACHE, &terms);
    }

    // ── the three conditions ──
    let conditions: Vec<(&str, Vec<Vec<String>>)> = vec![
        ("none", active.iter().map(|_| Vec::new()).collect()),
        (
            "curated (fixture)",
            active.iter().map(|m| m.retrieval.aliases.clone()).collect(),
        ),
        (
            "generated terms",
            active
                .iter()
                .map(|m| terms.get(&m.statement).cloned().unwrap_or_default())
                .collect(),
        ),
    ];

    // ── embed every condition's view ──
    let mut vectors_cache: HashMap<String, Vec<f32>> = load(EMBED_CACHE);
    let mut wanted: Vec<String> = Vec::new();
    for (_, aliases) in &conditions {
        for (memory, alias) in active.iter().zip(aliases) {
            wanted.push(view(memory, alias));
        }
    }
    wanted.retain(|text| !vectors_cache.contains_key(&embed_key(text)));
    wanted.sort();
    wanted.dedup();
    if !wanted.is_empty() {
        eprintln!("embedding {} new views…", wanted.len());
    }
    for chunk in wanted.chunks(CONCURRENCY) {
        let mut set = tokio::task::JoinSet::new();
        for text in chunk {
            let (client, key, text) = (client.clone(), key.clone(), text.clone());
            set.spawn(async move {
                let vector = embed(&client, &key, &text).await;
                (embed_key(&text), vector)
            });
        }
        while let Some(joined) = set.join_next().await {
            if let Ok((k, Some(vector))) = joined {
                vectors_cache.insert(k, vector);
            }
        }
        save(EMBED_CACHE, &vectors_cache);
    }

    // ── ask the same 93 questions under each condition ──
    let questions: Vec<(&'static str, &'static str)> = paraphrase::all()
        .map(|(probe, phrasing)| (probe, phrasing.query))
        .collect();
    let query_cache: HashMap<String, Vec<f32>> = load("semantic-width-embeddings.json");

    let mut rows: Vec<(String, Tally, Tally, Tally, usize)> = Vec::new();
    for (label, aliases) in &conditions {
        // BM25 over records carrying this condition's aliases.
        let shaped: Vec<CanonicalMemory> = active
            .iter()
            .zip(aliases)
            .map(|(memory, alias)| {
                let mut copy = memory.clone();
                copy.retrieval.aliases = alias.clone();
                copy
            })
            .collect();
        let index = MemoryIndex::build(shaped.iter().map(IndexedMemory::from_canonical));
        let ids: Vec<MemoryId> = shaped.iter().map(|m| m.id.clone()).collect();

        let vectors: Vec<Vec<f32>> = active
            .iter()
            .zip(aliases)
            .map(|(memory, alias)| {
                vectors_cache
                    .get(&embed_key(&view(memory, alias)))
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        if vectors.iter().any(Vec::is_empty) {
            eprintln!("SKIP: embeddings missing for condition {label}");
            return;
        }

        let added: usize =
            aliases.iter().map(|a| a.join(", ").len()).sum::<usize>() / active.len().max(1);

        let (mut lex, mut sem, mut fused) = (Tally::default(), Tally::default(), Tally::default());
        for (probe_name, question) in &questions {
            let probe = PROBES
                .iter()
                .find(|p| p.name == *probe_name)
                .expect("probe");
            let Some(query_vector) = query_cache.get(&stable_hash(&format!(
                "{EMBEDDING_MODEL}|{WIDTH}|RETRIEVAL_QUERY|{question}"
            ))) else {
                eprintln!("SKIP: query embeddings missing; run `semantic_fusion_probe` first.");
                return;
            };

            let lexical_hits = lexical(&index, question);
            let semantic_hits = as_hits(&semantic(query_vector, &vectors), &ids);
            let fused_hits = fuse(&[&lexical_hits, &semantic_hits, &semantic_hits]);

            lex.record(rank_of(&lexical_hits, probe.target));
            sem.record(rank_of(&semantic_hits, probe.target));
            fused.record(rank_of(&fused_hits, probe.target));
        }
        rows.push(((*label).to_string(), lex, sem, fused, added));
    }

    // Why it went the way it did: how much of the generated text is *new*?
    // An alias made of words the statement already contains adds length to a
    // length-normalised field and no reachability at all.
    let novelty = |aliases: &[Vec<String>]| -> (f64, f64) {
        let (mut novel, mut total, mut records_with_any) = (0usize, 0usize, 0usize);
        for (memory, alias) in active.iter().zip(aliases) {
            if alias.is_empty() {
                continue;
            }
            records_with_any += 1;
            let existing: std::collections::HashSet<String> =
                gemini_memory_rs::bm25::tokenize(&memory.statement)
                    .into_iter()
                    .collect();
            for term in alias {
                for token in gemini_memory_rs::bm25::tokenize(term) {
                    total += 1;
                    if !existing.contains(&token) {
                        novel += 1;
                    }
                }
            }
        }
        (
            if total == 0 {
                0.0
            } else {
                100.0 * novel as f64 / total as f64
            },
            100.0 * records_with_any as f64 / active.len() as f64,
        )
    };

    let sample = terms
        .get(&active[0].statement)
        .map(|t| t.join(", "))
        .unwrap_or_default();
    let mut report = format!(
        "\ndoes the alias field earn its place, and with what?\n\
         {} questions over {} records; top-5 (MRR)\n\n\
         e.g. {:?}\n  generated terms: {sample}\n\n\
         {:<20} {:<20} {:<20} {:<20} {}\n",
        questions.len(),
        active.len(),
        active[0].statement,
        "aliases",
        "lexical",
        "semantic",
        "fused 2:1",
        "chars added",
    );
    for (label, lex, sem, fused, added) in &rows {
        report.push_str(&format!(
            "{label:<20} {:<20} {:<20} {:<20} {added}\n",
            lex.cell(),
            sem.cell(),
            fused.cell(),
        ));
    }
    report.push_str(
        "\nwhy: how much of each alias set is a word the statement did not already have\n",
    );
    for (label, aliases) in &conditions {
        if aliases.iter().all(Vec::is_empty) {
            continue;
        }
        let (novel, coverage) = novelty(aliases);
        report.push_str(&format!(
            "  {label:<20} {novel:.0}% of alias tokens are new; {coverage:.0}% of records \
             carry any\n"
        ));
    }

    report.push_str(
        "\nFor scale: `query_rewrite_probe` bought lexical 58 -> 76 by expanding the query\n\
         with synonyms, at one model call per query on the path the user waits on. Anything\n\
         the `generated terms` row recovers is the same gain moved to ingestion, where it is\n\
         paid once per record and never again.\n",
    );
    eprintln!("{report}");

    let none = rows.iter().find(|(l, ..)| l == "none").expect("none row");
    let generated = rows
        .iter()
        .find(|(l, ..)| l == "generated terms")
        .expect("generated row");
    assert!(
        generated.2.asked > 0 && none.2.asked > 0,
        "a condition scored no questions at all\n{report}"
    );
}
