//! Whether rewriting the query helps, and which direction is the right one.
//!
//! # Why the direction is the whole question
//!
//! `semantic_fusion_probe` established the document side: what you embed
//! matters more than how you search it, and the winning text is the record's
//! own frontmatter written as prose. This is the same question asked of the
//! *query* side, and it matters for a reason the earlier files flagged and
//! could not settle.
//!
//! Every one of the 93 questions is phrased as a person speaks. In production
//! the model does not pass the utterance through — it writes a `recall_context`
//! query. `corpus::Probe` has carried both forms all along for exactly this
//! reason: `ask` is what the user says, `query` is what a model plausibly
//! writes. If the rewrite is doing the work, the case for a semantic layer is
//! weaker than every number in this crate suggests, because BM25 would be
//! getting a much easier query than the one it was measured on.
//!
//! # The four directions
//!
//! | direction | what it does | why it might work |
//! |---|---|---|
//! | `raw` | the utterance, untouched | the baseline every other file used |
//! | `third-person` | "the user's usual coffee order" | moves the query toward the corpus's own voice |
//! | `hyde` | writes a *hypothetical answer* and embeds that | moves the query into the document distribution |
//! | `expanded` | pads with plausible related context | the direction people reach for first |
//!
//! `hyde` is the interesting one. A dense retriever compares a question to a
//! statement, and those live in different parts of the space; writing a fake
//! statement first — "The user's usual coffee order is a flat white" — puts
//! both sides in the same shape, and the specifics being wrong matters less
//! than the shape being right. It is also the one direction that should help
//! semantic and *hurt* lexical, since the invented specifics are exactly the
//! wrong tokens for an inverted index.
//!
//! `expanded` is the control. Adding context is the reflex, and
//! `semantic_fusion_probe` already found that reflex wrong on the document
//! side — six generated questions per record made retrieval worse than the bare
//! statement. If it is wrong on the query side too, that is a general rule
//! rather than a coincidence.
//!
//! HyDE is also embedded twice, once as `RETRIEVAL_QUERY` and once as
//! `RETRIEVAL_DOCUMENT`, because if the rewrite really has made the query into
//! a document then the task type should follow it. That comparison turned out
//! to measure something else entirely — see
//! [`task_type_is_a_no_op_on_gemini_embedding_2`].
//!
//! # What it found
//!
//! | direction | lexical | semantic | fused 2:1 | chars |
//! |---|---|---|---|---|
//! | **raw** | 58/93 (0.536) | **73/93 (0.748)** | **79/93 (0.752)** | 31 |
//! | third-person | 52/93 (0.492) | 66/93 (0.648) | 66/93 (0.652) | 28 |
//! | hyde | 57/93 (0.552) | 64/93 (0.643) | 65/93 (0.620) | 52 |
//! | expanded | **76/93 (0.686)** | 72/93 (0.735) | 77/93 (0.779) | 181 |
//!
//! **Rewriting for the semantic side does not work.** Every rewrite loses
//! ground against the raw utterance: 73 → 66, 64, 72. The embedding model is
//! already doing the paraphrase work, and rewriting first throws away signal it
//! was using.
//!
//! **HyDE — the direction with the best theory behind it — is the worst.** 64
//! of 93 against the raw 73. Writing a plausible-sounding fake answer replaces
//! the specifics that identify the record with specifics that identify nothing:
//! "The user's usual coffee order is a flat white" is a confident, fluent,
//! well-shaped statement about the wrong drink, and it retrieves the wrong
//! drink.
//!
//! **Expansion is the opposite of what I predicted, and only for lexical.**
//! It was the control — the direction people reach for and the one
//! `semantic_fusion_probe` found wrong on the document side. On the query side
//! it takes BM25 from 58 to **76**, an eighteen-question gain, because synonyms
//! are exactly what an inverted index cannot generate for itself. It does
//! nothing for the semantic side (72 against 73) for the same reason in reverse.
//!
//! And it still does not win the fused pipeline: 77 against the raw 79 on
//! top-5, though it takes the best MRR at 0.779. Fusion had already recovered
//! most of what expansion recovers, so the two overlap rather than add.
//!
//! **The gap this file was written to close, closed the other way.** The worry
//! was that a model rewriting queries would hand BM25 something much easier
//! than the utterances every measurement in this crate used, making the lexical
//! baseline of 58 flattering to the semantic case. It does the reverse:
//! third-person rewriting takes lexical *down* to 52. The baselines were not
//! optimistic.
//!
//! **So: rewrite for the lexical index or not at all.** If the system were
//! BM25-only, an expansion pass would be the single highest-value change
//! available — bigger than anything else measured here. In a fused system it is
//! 181 characters and a model call to lose two questions at top-5 and gain
//! 0.027 MRR, which is not a trade worth the round trip.
//!
//! Rewrites and embeddings are cached by content hash. Skips without a key.

#![cfg(feature = "gemini-llm")]

mod common;

use std::collections::HashMap;

use common::corpus::{self, PROBES};
use common::paraphrase;
use common::rank::{as_hits, fuse, lexical, rank_of, CANDIDATES};
use common::views::structural_view;
use common::{file_backed_engine, have_api_key, skip, ScratchDir};

use gemini_memory_rs::bm25::{IndexedMemory, MemoryIndex};
use gemini_memory_rs::core::{stable_hash, CanonicalMemory, MemoryId, MemoryStatus};

const EMBEDDING_MODEL: &str = "gemini-embedding-2";
const WIDTH: usize = 768;
const DOC_CACHE: &str = "semantic-width-embeddings.json";
const REWRITE_CACHE: &str = "query-rewrite-cache.json";
const MODEL: &str = "gemini-2.5-flash-lite";
const CONCURRENCY: usize = 12;

/// The rewrite directions, and the instruction that produces each.
const DIRECTIONS: &[(&str, &str)] = &[
    (
        "third-person",
        "Rewrite it as the search query an assistant would use to look this up in \
         the user's stored memory: third person, about \"the user\", naming the \
         attribute being asked for. One line, no quotes, no explanation.",
    ),
    (
        "hyde",
        "Write the single sentence that this stored memory would contain if it \
         answered the question — a plain statement of fact about the user, in the \
         third person. Invent a specific plausible value. One line, no quotes, no \
         explanation.",
    ),
    (
        "expanded",
        "Rewrite it as a longer search query, adding related terms, synonyms and \
         plausible surrounding context that might help find the answer. Two or \
         three lines, no explanation.",
    ),
];

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

/// Rewrite one question. Sampling is left at the model's default throughout.
async fn rewrite(
    client: &reqwest::Client,
    key: &str,
    question: &str,
    instruction: &str,
) -> Option<String> {
    let url =
        format!("https://generativelanguage.googleapis.com/v1beta/models/{MODEL}:generateContent");
    let body = serde_json::json!({
        "contents": [{ "role": "user", "parts": [{
            "text": format!("A user said: \"{question}\"\n\n{instruction}")
        }]}],
        "generationConfig": { "maxOutputTokens": 2048 },
    });
    let mut backoff = std::time::Duration::from_millis(500);
    for attempt in 0..5 {
        if let Ok(response) = client
            .post(&url)
            .header("x-goog-api-key", key)
            .json(&body)
            .send()
            .await
        {
            if response.status().is_success() {
                let json: serde_json::Value = response.json().await.ok()?;
                let text = json["candidates"][0]["content"]["parts"]
                    .as_array()?
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("");
                let trimmed = text.trim().trim_matches('"').trim().to_string();
                return (!trimmed.is_empty()).then_some(trimmed);
            }
            if attempt == 4 {
                eprintln!("  rewrite {} — giving up", response.status());
            }
        }
        tokio::time::sleep(backoff).await;
        backoff *= 2;
    }
    None
}

async fn embed(client: &reqwest::Client, key: &str, text: &str, task: &str) -> Option<Vec<f32>> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{EMBEDDING_MODEL}:embedContent"
    );
    let body = serde_json::json!({
        "model": format!("models/{EMBEDDING_MODEL}"),
        "content": { "parts": [{ "text": text }] },
        "taskType": task,
        "outputDimensionality": WIDTH,
    });
    let mut backoff = std::time::Duration::from_millis(500);
    for _ in 0..5 {
        if let Ok(response) = client
            .post(&url)
            .header("x-goog-api-key", key)
            .json(&body)
            .send()
            .await
        {
            if response.status().is_success() {
                let json: serde_json::Value = response.json().await.ok()?;
                let values = json["embedding"]["values"].as_array()?;
                let mut vector: Vec<f32> = values
                    .iter()
                    .filter_map(|v| v.as_f64())
                    .map(|v| v as f32)
                    .collect();
                let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
                if norm > 0.0 {
                    for value in &mut vector {
                        *value /= norm;
                    }
                }
                return Some(vector);
            }
        }
        tokio::time::sleep(backoff).await;
        backoff *= 2;
    }
    None
}

fn doc_key(task: &str, text: &str) -> String {
    stable_hash(&format!("{EMBEDDING_MODEL}|{WIDTH}|{task}|{text}"))
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
        format!("{}/{}", self.top_five, self.asked)
    }
    fn mrr(&self) -> f64 {
        self.reciprocal / self.asked.max(1) as f64
    }
}

#[tokio::test]
async fn which_direction_of_query_rewrite_actually_helps() {
    if !have_api_key() {
        return skip("which_direction_of_query_rewrite_actually_helps");
    }
    let docs: HashMap<String, Vec<f32>> = load(DOC_CACHE);
    if docs.is_empty() {
        eprintln!("SKIP: no document embeddings; run `semantic_fusion_probe` first.");
        return;
    }

    let scratch = ScratchDir::new("query-rewrite");
    let engine = file_backed_engine("usr_corpus", scratch.path());
    corpus::install(&engine).await;
    let records = corpus::installed(&engine).await;
    let active: Vec<&CanonicalMemory> = records
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .collect();

    let mut vectors = Vec::with_capacity(active.len());
    for memory in &active {
        let Some(vector) = docs.get(&doc_key("RETRIEVAL_DOCUMENT", &structural_view(memory)))
        else {
            eprintln!("SKIP: cache missing the structural view at {WIDTH}d.");
            return;
        };
        vectors.push(vector.clone());
    }
    let index = MemoryIndex::build(active.iter().map(|m| IndexedMemory::from_canonical(m)));
    let ids: Vec<MemoryId> = active.iter().map(|m| m.id.clone()).collect();

    let questions: Vec<(&'static str, &'static str)> = paraphrase::all()
        .map(|(probe, phrasing)| (probe, phrasing.query))
        .collect();

    let client = reqwest::Client::new();
    let key = api_key();

    // ── rewrite every question in every direction ──
    let mut rewrites: HashMap<String, String> = load(REWRITE_CACHE);
    let wanted: Vec<(String, String, String)> = questions
        .iter()
        .flat_map(|(_, q)| {
            DIRECTIONS.iter().map(move |(name, instruction)| {
                (
                    stable_hash(&format!("{MODEL}|{name}|{q}")),
                    q.to_string(),
                    instruction.to_string(),
                )
            })
        })
        .filter(|(k, _, _)| !rewrites.contains_key(k))
        .collect();
    for chunk in wanted.chunks(CONCURRENCY) {
        let mut set = tokio::task::JoinSet::new();
        for (k, question, instruction) in chunk {
            let (client, key, k, question, instruction) = (
                client.clone(),
                key.clone(),
                k.clone(),
                question.clone(),
                instruction.clone(),
            );
            set.spawn(async move { (k, rewrite(&client, &key, &question, &instruction).await) });
        }
        while let Some(joined) = set.join_next().await {
            if let Ok((k, Some(text))) = joined {
                rewrites.insert(k, text);
            }
        }
        save(REWRITE_CACHE, &rewrites);
    }

    // ── embed each rewrite; HyDE twice, as query and as document ──
    let mut query_vectors: HashMap<String, Vec<f32>> = load("query-rewrite-embeddings.json");
    let mut to_embed: Vec<(String, String)> = Vec::new();
    for (_, question) in &questions {
        to_embed.push(("RETRIEVAL_QUERY".into(), question.to_string()));
        for (name, _) in DIRECTIONS {
            let Some(text) = rewrites.get(&stable_hash(&format!("{MODEL}|{name}|{question}")))
            else {
                continue;
            };
            to_embed.push(("RETRIEVAL_QUERY".into(), text.clone()));
            if *name == "hyde" {
                to_embed.push(("RETRIEVAL_DOCUMENT".into(), text.clone()));
            }
        }
    }
    to_embed.retain(|(task, text)| !query_vectors.contains_key(&doc_key(task, text)));
    to_embed.sort();
    to_embed.dedup();
    for chunk in to_embed.chunks(CONCURRENCY) {
        let mut set = tokio::task::JoinSet::new();
        for (task, text) in chunk {
            let (client, key, task, text) =
                (client.clone(), key.clone(), task.clone(), text.clone());
            set.spawn(async move {
                let vector = embed(&client, &key, &text, &task).await;
                (doc_key(&task, &text), vector)
            });
        }
        while let Some(joined) = set.join_next().await {
            if let Ok((k, Some(vector))) = joined {
                query_vectors.insert(k, vector);
            }
        }
        save("query-rewrite-embeddings.json", &query_vectors);
    }

    // ── score every direction on all three retrievers ──
    let mut rows: Vec<(String, Tally, Tally, Tally, usize)> = Vec::new();
    let mut variants: Vec<(String, &str)> = vec![("raw".into(), "RETRIEVAL_QUERY")];
    for (name, _) in DIRECTIONS {
        variants.push(((*name).to_string(), "RETRIEVAL_QUERY"));
    }
    variants.push(("hyde as document".into(), "RETRIEVAL_DOCUMENT"));

    for (label, task) in &variants {
        let (mut lex, mut sem, mut fused) = (Tally::default(), Tally::default(), Tally::default());
        let mut chars = 0usize;
        for (probe_name, question) in &questions {
            let probe = PROBES
                .iter()
                .find(|p| p.name == *probe_name)
                .expect("probe");
            let text: String = if label == "raw" {
                question.to_string()
            } else {
                let name = label.split(' ').next().unwrap_or(label);
                match rewrites.get(&stable_hash(&format!("{MODEL}|{name}|{question}"))) {
                    Some(t) => t.clone(),
                    None => continue,
                }
            };
            let Some(vector) = query_vectors.get(&doc_key(task, &text)) else {
                continue;
            };
            chars += text.chars().count();

            let lexical_hits = lexical(&index, &text);
            let semantic_hits = as_hits(&semantic(vector, &vectors), &ids);
            let fused_hits = fuse(&[&lexical_hits, &semantic_hits, &semantic_hits]);

            lex.record(rank_of(&lexical_hits, probe.target));
            sem.record(rank_of(&semantic_hits, probe.target));
            fused.record(rank_of(&fused_hits, probe.target));
        }
        let mean_chars = chars / lex.asked.max(1);
        rows.push((label.clone(), lex, sem, fused, mean_chars));
    }

    let mut report = format!(
        "\nwhich direction of query rewrite helps, over {} questions and {} records\n\
         all three retrievers see the same rewritten text; top-5 and MRR\n\n\
         {:<20} {:<18} {:<18} {:<18} {}\n",
        questions.len(),
        active.len(),
        "direction",
        "lexical",
        "semantic",
        "fused 2:1",
        "chars",
    );
    for (label, lex, sem, fused, chars) in &rows {
        report.push_str(&format!(
            "{label:<20} {:<18} {:<18} {:<18} {chars}\n",
            format!("{} ({:.3})", lex.cell(), lex.mrr()),
            format!("{} ({:.3})", sem.cell(), sem.mrr()),
            format!("{} ({:.3})", fused.cell(), fused.mrr()),
        ));
    }
    eprintln!("{report}");

    assert!(
        rows.iter().all(|(_, l, _, _, _)| l.asked > 0),
        "a direction produced no scored questions at all\n{report}"
    );
}

/// `gemini-embedding-2` ignores `taskType`. `gemini-embedding-001` does not.
///
/// Found by accident: the HyDE rewrite was embedded twice, once as a query and
/// once as a document, and the two produced byte-identical rankings. They
/// produced byte-identical *vectors*.
///
/// This matters because every embedding call in this crate carefully passes
/// `RETRIEVAL_DOCUMENT` for records and `RETRIEVAL_QUERY` for questions, on the
/// documented reasoning that a stored fact and a question about it should land
/// in deliberately different places. On `gemini-embedding-2` that has been a
/// no-op throughout. It invalidates no measurement — both sides passed the same
/// ignored parameter — but it does invalidate the explanation, and an
/// explanation nobody checks is how a cargo-cult parameter survives for years.
///
/// Pinned as a test rather than a comment so that if the model starts honouring
/// it, the asymmetry becomes available again and somebody finds out.
#[tokio::test]
async fn task_type_is_a_no_op_on_gemini_embedding_2() {
    if !have_api_key() {
        return skip("task_type_is_a_no_op_on_gemini_embedding_2");
    }
    let client = reqwest::Client::new();
    let key = api_key();
    let text = "what coffee do I usually order";

    let as_query = embed(&client, &key, text, "RETRIEVAL_QUERY").await;
    let as_document = embed(&client, &key, text, "RETRIEVAL_DOCUMENT").await;
    let (Some(as_query), Some(as_document)) = (as_query, as_document) else {
        eprintln!("SKIP: embedding calls failed");
        return;
    };

    let cosine: f32 = as_query.iter().zip(&as_document).map(|(a, b)| a * b).sum();
    eprintln!(
        "\ngemini-embedding-2, same text under two task types: cosine {cosine:.6}\n\
         (1.000000 means the parameter is ignored)\n"
    );

    assert!(
        (cosine - 1.0).abs() < 1e-4,
        "gemini-embedding-2 has started honouring `taskType` — cosine between the \
         query and document embeddings of the same text is {cosine:.6}. The \
         asymmetry is real again, which means it is worth measuring whether using \
         it improves retrieval, and the note in `semantic_fusion_probe` saying it \
         is a no-op should be revised."
    );
}
