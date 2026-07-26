//! What the two serving models actually cost, in wall-clock, against the
//! budgets the engine sets.
//!
//! # Why this decides the architecture rather than tuning it
//!
//! `speculation_gate` establishes that a semantic backend now reaches the model
//! on both paths. It says nothing about whether a *remote* backend can answer
//! in time, and the engine's two budgets are three and a half orders of
//! magnitude apart from a network round trip:
//!
//! | path | semantic budget | who is waiting |
//! |---|---|---|
//! | `recall_context` (`immediate_semantic_timeout_ms`) | 10 ms | the model, mid-turn |
//! | speculative (`semantic_fallback_timeout_ms`) | 100 ms | nobody |
//!
//! A dense retriever cannot search until the *query* is embedded, and embedding
//! the query is an API call. So the number measured here decides whether the
//! semantic layer can be consulted when the model asks, or only in advance —
//! and "only in advance" is a different product, because it means the thing
//! being embedded is the transcript rather than the question.
//!
//! # What is measured
//!
//! **Embeddings** (`embedContent`) return one JSON body, so there is no
//! meaningful time-to-first-token: TTFT *is* the total, and saying otherwise
//! would be theatre. What varies and is worth knowing is the width, the input
//! length, and what concurrency buys, since ingestion embeds a corpus and
//! retrieval embeds one string.
//!
//! **Generation** (`streamGenerateContent`) does stream, so TTFT and the decode
//! rate separate cleanly — and they answer different questions. TTFT is prefill
//! and queueing: it is what an out-of-band extractor pays before it can do
//! anything. Tokens per second is decode: it is what a long structured output
//! costs on top. A 2,000-token extraction and a 20-token one have nearly the
//! same TTFT and wildly different totals.
//!
//! # The caveat that applies to every number here
//!
//! These run through whatever network the test host has, including the agent
//! proxy in CI. They are *deployment* latencies, not the model's service time,
//! and a server in the same region as the API will see less. That is the right
//! quantity for deciding budgets — the budget has to hold where the code runs —
//! but it is the wrong quantity for comparing models to a published benchmark.
//!
//! Skips entirely without an API key. Nothing here is asserted as a threshold
//! except the one structural claim the architecture depends on.
//!
//! # What it found
//!
//! **Embedding, `gemini-embedding-2`.** 259 ms p50 warm, and a 504 ms TLS
//! handshake paid once per connection — so a client that is not reused doubles
//! the cost of the first call. Latency is **flat across widths**: 253 ms at
//! 768d, 242 ms at 1536d, 263 ms at 3072d. It is also flat across input length
//! over the range that matters (42 to 404 characters). Concurrency scales
//! close to linearly to 88 embeds/s at ×32, so seeding a 16,000-record corpus
//! is about three minutes of wall-clock rather than an overnight job.
//!
//! **Generation, streamed.** TTFT and decode separate cleanly:
//!
//! | model | TTFT p50 | total p50 (≈180 tok) | decode |
//! |---|---|---|---|
//! | `gemini-3.5-flash-lite` | 368 ms | 985 ms | 319 tok/s |
//! | `gemini-2.5-flash-lite` | 236 ms | 905 ms | 283 tok/s |
//!
//! 3.5-flash-lite is *slower to first token* and faster to decode, which
//! roughly cancels on a 180-token output. For the memory engine that trade is
//! the wrong way round: both model calls here emit small structured objects,
//! so they are priced by TTFT rather than by decode, and 2.5-flash-lite is 130
//! ms cheaper on exactly the thing that dominates.
//!
//! **The consequence for the semantic layer.** One embedding costs 259 ms
//! against budgets of 10 ms and 100 ms. Nothing tunes that away — it is a
//! network round trip against a mid-turn deadline. A dense retriever cannot
//! search until the query is embedded, so a *remote* backend cannot be
//! consulted from the model's question at all. The two ways out are to embed
//! locally, or to accept that the thing being embedded is the transcript
//! during speculation rather than the question at recall time. That is a
//! product decision — it changes what the retriever is answering — and it is
//! forced by this number rather than chosen.

#![cfg(feature = "gemini-llm")]

mod common;

use std::time::{Duration, Instant};

use common::{have_api_key, skip};

/// The embedding model, and the widths worth comparing.
const EMBED_MODEL: &str = "gemini-embedding-2";
const WIDTHS: &[usize] = &[768, 1536, 3072];

/// The generation models. `gemini-2.5-flash-lite` is what
/// `semantic_fusion_probe` used for enrichment, so the delta is the cost of
/// moving to the current generation.
const GEN_MODELS: &[&str] = &["gemini-3.5-flash-lite", "gemini-2.5-flash-lite"];

/// Concurrency levels for the ingestion-shaped sweep.
const CONCURRENCIES: &[usize] = &[1, 4, 8, 16, 32];

/// How many samples per cell. Small because every one is a paid round trip;
/// large enough that a p50 means something.
const SAMPLES: usize = 12;

/// Three input lengths, matching the views `semantic_fusion_probe` measured.
const SHORT: &str = "The user's barber is Deepa at Tuloma Salon.";
const MEDIUM: &str = "The user's barber is Deepa at Tuloma Salon.\nAbout: user\n\
     Kind: Preference barber\nMentions: i, me, my, user\nHolds: Persistent";
const LONG: &str = "The user's barber is Deepa at Tuloma Salon.\n\
     Also asked as: who cuts my hair, my barber\nTopics: barber, haircut, hair, salon\n\
     About: user\nKind: Preference barber\nMentions: i, me, my, user\nHolds: Persistent\n\
     Book me a trim at Tuloma.\nCan you schedule my haircut with Deepa?\n\
     I need to see my stylist.\nRemind me about my appointment at the salon.\n\
     Is Deepa available next week?\nGet me in for a cut.";

fn api_key() -> String {
    ["GEMINI_API_KEY", "GOOGLE_GENAI_API_KEY", "GOOGLE_API_KEY"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.trim().is_empty())
        .expect("an API key, checked by the caller")
}

// ─── statistics ─────────────────────────────────────────────────────────────

/// Percentiles over a set of durations, in milliseconds.
struct Stats {
    n: usize,
    p50: f64,
    p95: f64,
    max: f64,
    mean: f64,
}

impl Stats {
    fn of(mut samples: Vec<Duration>) -> Self {
        assert!(!samples.is_empty(), "no samples");
        samples.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let at = |q: f64| ms(samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)]);
        Self {
            n: samples.len(),
            p50: at(0.50),
            p95: at(0.95),
            max: ms(*samples.last().expect("samples")),
            mean: samples.iter().map(|d| ms(*d)).sum::<f64>() / samples.len() as f64,
        }
    }

    fn row(&self, label: &str) -> String {
        format!(
            "{label:<28} n={:<3} p50={:>8.0}ms p95={:>8.0}ms max={:>8.0}ms mean={:>8.0}ms\n",
            self.n, self.p50, self.p95, self.max, self.mean
        )
    }
}

// ─── embeddings ─────────────────────────────────────────────────────────────

/// One embedding call. Returns the wall-clock and the vector's length, so a
/// silently-ignored `outputDimensionality` cannot pass as a fast result.
async fn embed_once(
    client: &reqwest::Client,
    key: &str,
    text: &str,
    dims: usize,
) -> Option<(Duration, usize)> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{EMBED_MODEL}:embedContent"
    );
    let body = serde_json::json!({
        "model": format!("models/{EMBED_MODEL}"),
        "content": { "parts": [{ "text": text }] },
        "taskType": "RETRIEVAL_DOCUMENT",
        "outputDimensionality": dims,
    });

    let started = Instant::now();
    let response = client
        .post(&url)
        .header("x-goog-api-key", key)
        .json(&body)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        eprintln!("  embed {} — sample dropped", response.status());
        return None;
    }
    let json: serde_json::Value = response.json().await.ok()?;
    let elapsed = started.elapsed();
    let width = json["embedding"]["values"].as_array()?.len();
    Some((elapsed, width))
}

// ─── generation ─────────────────────────────────────────────────────────────

/// One streamed generation, decomposed.
struct Generation {
    /// Time until the first chunk carrying text — prefill plus queueing.
    ttft: Duration,
    /// Time until the stream closes.
    total: Duration,
    /// Output tokens, from the final chunk's `usageMetadata`.
    output_tokens: usize,
    /// Input tokens, likewise.
    input_tokens: usize,
}

impl Generation {
    /// Decode rate: output tokens divided by the time spent decoding them.
    ///
    /// Deliberately not `output / total`, which conflates prefill with decode
    /// and makes a fast model look slow on short outputs.
    ///
    /// `None` when the whole output arrived inside a single chunk. The decode
    /// window is then somewhere between zero and one network read, and dividing
    /// by it produces numbers like 29,000 tok/s — which is not a fast model, it
    /// is a measurement with no denominator.
    fn decode_tps(&self) -> Option<f64> {
        let decoding = self.total.saturating_sub(self.ttft);
        if decoding < Duration::from_millis(10) {
            return None;
        }
        Some(self.output_tokens as f64 / decoding.as_secs_f64())
    }

    /// What the caller actually experiences: tokens divided by the whole wait.
    fn effective_tps(&self) -> f64 {
        let total = self.total.as_secs_f64();
        if total <= 0.0 {
            return 0.0;
        }
        self.output_tokens as f64 / total
    }
}

/// Stream a generation, timing the first text chunk and the last.
async fn generate_once(
    client: &reqwest::Client,
    key: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
) -> Option<Generation> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/\
         {model}:streamGenerateContent?alt=sse"
    );
    let body = serde_json::json!({
        "contents": [{ "role": "user", "parts": [{ "text": prompt }] }],
        "generationConfig": { "temperature": 0.4, "maxOutputTokens": max_tokens },
    });

    let started = Instant::now();
    let mut response = client
        .post(&url)
        .header("x-goog-api-key", key)
        .json(&body)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        eprintln!("  generate {} — sample dropped", response.status());
        return None;
    }

    let mut ttft = None;
    let (mut output_tokens, mut input_tokens) = (0usize, 0usize);
    let mut pending = String::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        pending.push_str(&String::from_utf8_lossy(&chunk));
        // SSE frames are `data: {json}` on their own line. Split on newlines
        // rather than on a blank-line separator: the API terminates frames with
        // CRLF, so looking for "\n\n" matches nothing and silently reports every
        // stream as a single chunk.
        while let Some(end) = pending.find('\n') {
            let line: String = pending.drain(..end + 1).collect();
            let Some(payload) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let Ok(json) = serde_json::from_str::<serde_json::Value>(payload.trim()) else {
                continue;
            };
            let has_text = json["candidates"][0]["content"]["parts"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(|p| p["text"].as_str().is_some()));
            if has_text && ttft.is_none() {
                ttft = Some(started.elapsed());
            }
            if let Some(usage) = json["usageMetadata"].as_object() {
                if let Some(n) = usage.get("candidatesTokenCount").and_then(|v| v.as_u64()) {
                    output_tokens = n as usize;
                }
                if let Some(n) = usage.get("promptTokenCount").and_then(|v| v.as_u64()) {
                    input_tokens = n as usize;
                }
            }
        }
    }
    let total = started.elapsed();

    Some(Generation {
        // A stream that produced usage but never a text part still took time to
        // first *something*; fall back to the total rather than dropping it.
        ttft: ttft.unwrap_or(total),
        total,
        output_tokens,
        input_tokens,
    })
}

// ─── the measurements ───────────────────────────────────────────────────────

/// What an embedding costs, by width, by input length, and under concurrency.
#[tokio::test]
async fn embedding_serving_latency() {
    if !have_api_key() {
        return skip("embedding_serving_latency");
    }
    let client = reqwest::Client::new();
    let key = api_key();

    let mut report = String::from(
        "\nembedding latency — gemini-embedding-2, via this host's network\n\n\
         `embedContent` returns a single body, so TTFT is the total; there is no\n\
         prefill/decode split to report.\n\n",
    );

    // ── cold vs warm: the first call pays DNS, TCP and TLS ──
    let cold = embed_once(&client, &key, SHORT, 768).await;
    let mut warm = Vec::new();
    for _ in 0..SAMPLES {
        if let Some((elapsed, _)) = embed_once(&client, &key, SHORT, 768).await {
            warm.push(elapsed);
        }
    }
    if let (Some((cold, _)), false) = (cold, warm.is_empty()) {
        let warm_stats = Stats::of(warm.clone());
        report.push_str(&format!(
            "connection setup\n  \
             first call on a new client : {:.0}ms\n  \
             subsequent, keep-alive     : {:.0}ms p50\n  \
             handshake cost             : ~{:.0}ms, paid once per connection\n\n",
            cold.as_secs_f64() * 1000.0,
            warm_stats.p50,
            (cold.as_secs_f64() * 1000.0 - warm_stats.p50).max(0.0),
        ));
    }

    // ── width ──
    report.push_str("by output width, same 42-character input\n");
    let mut by_width = Vec::new();
    for &dims in WIDTHS {
        let mut samples = Vec::new();
        let mut got_width = 0;
        for _ in 0..SAMPLES {
            if let Some((elapsed, width)) = embed_once(&client, &key, SHORT, dims).await {
                samples.push(elapsed);
                got_width = width;
            }
        }
        if samples.is_empty() {
            continue;
        }
        let stats = Stats::of(samples);
        report.push_str(&stats.row(&format!("{dims}d (returned {got_width})")));
        by_width.push((dims, stats.p50));
    }
    report.push('\n');

    // ── input length ──
    report.push_str("by input length, 768d\n");
    for (label, text) in [
        ("statement (42 chars)", SHORT),
        ("structural (136 chars)", MEDIUM),
        ("full (404 chars)", LONG),
    ] {
        let mut samples = Vec::new();
        for _ in 0..SAMPLES {
            if let Some((elapsed, _)) = embed_once(&client, &key, text, 768).await {
                samples.push(elapsed);
            }
        }
        if !samples.is_empty() {
            report.push_str(&Stats::of(samples).row(label));
        }
    }
    report.push('\n');

    // ── concurrency: what ingestion can actually push ──
    report.push_str(
        "by concurrency — the ingestion question, 768d\n\
         (per-call latency is expected to rise; the column that matters is throughput)\n",
    );
    for &concurrency in CONCURRENCIES {
        let started = Instant::now();
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..concurrency * 2 {
            let (client, key) = (client.clone(), key.clone());
            set.spawn(async move { embed_once(&client, &key, MEDIUM, 768).await });
        }
        let mut samples = Vec::new();
        while let Some(joined) = set.join_next().await {
            if let Ok(Some((elapsed, _))) = joined {
                samples.push(elapsed);
            }
        }
        let wall = started.elapsed().as_secs_f64();
        if samples.is_empty() {
            report.push_str(&format!(
                "{:<28} all calls failed\n",
                format!("×{concurrency}")
            ));
            continue;
        }
        let done = samples.len();
        let stats = Stats::of(samples);
        report.push_str(&format!(
            "{:<28} n={:<3} p50={:>8.0}ms p95={:>8.0}ms  →{:>7.1} embeds/s\n",
            format!("×{concurrency}"),
            stats.n,
            stats.p50,
            stats.p95,
            done as f64 / wall,
        ));
    }

    // ── the verdict the budgets need ──
    let interactive =
        gemini_memory_rs::core::RetrievalConfig::default().immediate_semantic_timeout_ms;
    let speculative =
        gemini_memory_rs::core::RetrievalConfig::default().semantic_fallback_timeout_ms;
    if !warm.is_empty() {
        let p50 = Stats::of(warm).p50;
        report.push_str(&format!(
            "\nagainst the engine's budgets\n  \
             one warm embedding, p50            : {p50:.0}ms\n  \
             `immediate_semantic_timeout_ms`    : {interactive}ms  → {}\n  \
             `semantic_fallback_timeout_ms`     : {speculative}ms  → {}\n",
            if p50 <= interactive as f64 {
                "fits"
            } else {
                "DOES NOT FIT"
            },
            if p50 <= speculative as f64 {
                "fits"
            } else {
                "DOES NOT FIT"
            },
        ));
        report.push_str(
            "\nA dense retriever cannot search until the query is embedded, so this number is\n\
             the floor on any remote semantic backend. Where it exceeds both budgets, the\n\
             backend cannot be consulted from the query at all — the embedding has to happen\n\
             off the critical path, on the transcript, during speculation. That is an\n\
             architectural consequence rather than a tuning one: what gets embedded stops\n\
             being the model's question and becomes what the user just said.\n\
             \n\
             A local backend is the other way out. `semantic_fusion_probe` measured an exact\n\
             flat scan over 1,199 vectors at 768d at 708µs, which fits either budget with\n\
             room to spare — but only once the vectors are already in memory, which still\n\
             leaves the query embedding to pay for.\n",
        );
    }
    eprintln!("{report}");
}

/// What a flash-lite call costs, split into prefill and decode.
#[tokio::test]
async fn generation_serving_latency() {
    if !have_api_key() {
        return skip("generation_serving_latency");
    }
    let client = reqwest::Client::new();
    let key = api_key();

    // Two shapes with the same prompt: one that stops almost immediately and
    // one that runs on. The difference isolates decode from prefill.
    let short_prompt = "Reply with exactly one word: acknowledged.";
    let long_prompt = "A voice assistant remembers this fact about its user:\n\n\
        The user's barber is Deepa at Tuloma Salon.\n\n\
        Write twenty short questions this fact would answer, as the user would \
        say them out loud. One per line, no numbering.";

    let mut report = String::from(
        "\ngeneration latency — streamed, via this host's network\n\n\
         TTFT is prefill plus queueing: what an out-of-band extractor pays before it can\n\
         do anything at all. decode tok/s is the marginal cost of a longer answer.\n\n",
    );

    for &model in GEN_MODELS {
        report.push_str(&format!("── {model} ──\n"));
        for (label, prompt, cap) in [
            ("short output", short_prompt, 32u32),
            ("long output", long_prompt, 2048),
        ] {
            let mut ttfts = Vec::new();
            let mut totals = Vec::new();
            let (mut decode, mut effective) = (Vec::new(), Vec::new());
            let (mut out_tokens, mut in_tokens) = (Vec::new(), Vec::new());

            for _ in 0..SAMPLES.min(8) {
                let Some(generation) = generate_once(&client, &key, model, prompt, cap).await
                else {
                    continue;
                };
                ttfts.push(generation.ttft);
                totals.push(generation.total);
                if let Some(tps) = generation.decode_tps() {
                    decode.push(tps);
                }
                effective.push(generation.effective_tps());
                out_tokens.push(generation.output_tokens);
                in_tokens.push(generation.input_tokens);
            }
            if ttfts.is_empty() {
                report.push_str(&format!("  {label:<14} all calls failed\n"));
                continue;
            }

            let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
            let mean_u = |v: &[usize]| v.iter().sum::<usize>() as f64 / v.len() as f64;
            report.push_str(&format!(
                "  {label} — {:.0} in, {:.0} out tokens (mean)\n",
                mean_u(&in_tokens),
                mean_u(&out_tokens),
            ));
            report.push_str(&format!("    {}", Stats::of(ttfts).row("TTFT")));
            report.push_str(&format!("    {}", Stats::of(totals).row("total")));
            let decode_cell = if decode.is_empty() {
                // Every sample fitted in one chunk: honest to say so.
                "  n/a (single chunk)".to_string()
            } else {
                format!("{:>6.1} tok/s", mean(&decode))
            };
            report.push_str(&format!(
                "    {:<28} decode {decode_cell}   effective {:>6.1} tok/s\n",
                "throughput",
                mean(&effective),
            ));
        }
        report.push('\n');
    }

    report.push_str(
        "Reading this for the pipeline: TTFT is what a per-turn extractor pays whether it\n\
         emits ten tokens or a thousand, so a call that fires every turn is priced by TTFT\n\
         and a call that emits a long structured object is priced by decode. The engine's\n\
         plan extractor is bounded at 4s and its observation extractor by the ingestion\n\
         soft timeout; both are off the response path, which is why the totals here are\n\
         affordable and the TTFT is the number to watch if either ever moves onto it.\n",
    );
    eprintln!("{report}");
}
