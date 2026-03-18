//! Cookbook #27 — RaceTextAgent + TimeoutTextAgent
//!
//! Demonstrates competitive execution patterns:
//!   - RaceTextAgent: run agents concurrently, first result wins, cancel the rest
//!   - TimeoutTextAgent: wrap any agent with a time limit
//!   - Combining race + timeout for production resilience

use adk_rs_fluent::prelude::*;
use std::sync::Arc;
use std::time::Duration;

// A mock LLM with configurable latency.
struct DelayLlm {
    name: String,
    delay: Duration,
    response: String,
}

#[async_trait::async_trait]
impl BaseLlm for DelayLlm {
    fn model_id(&self) -> &str { &self.name }
    async fn generate(
        &self,
        _req: rs_adk::llm::LlmRequest,
    ) -> Result<rs_adk::llm::LlmResponse, rs_adk::llm::LlmError> {
        tokio::time::sleep(self.delay).await;
        Ok(rs_adk::llm::LlmResponse {
            content: rs_genai::prelude::Content {
                role: Some(rs_genai::prelude::Role::Model),
                parts: vec![rs_genai::prelude::Part::Text {
                    text: self.response.clone(),
                }],
            },
            finish_reason: Some("STOP".into()),
            usage: None,
        })
    }
}

#[tokio::main]
async fn main() {
    println!("=== Cookbook #27: RaceTextAgent + TimeoutTextAgent ===\n");

    // ── 1. Basic race: fast vs slow agent ──
    println!("--- 1. Basic Race ---\n");

    let fast_llm: Arc<dyn BaseLlm> = Arc::new(DelayLlm {
        name: "fast".into(),
        delay: Duration::from_millis(50),
        response: "Fast agent response (50ms)".into(),
    });

    let slow_llm: Arc<dyn BaseLlm> = Arc::new(DelayLlm {
        name: "slow".into(),
        delay: Duration::from_millis(200),
        response: "Slow agent response (200ms)".into(),
    });

    let fast_agent = AgentBuilder::new("fast-responder")
        .instruction("Quick response")
        .build(fast_llm.clone());

    let slow_agent = AgentBuilder::new("slow-responder")
        .instruction("Detailed response")
        .build(slow_llm.clone());

    let race = RaceTextAgent::new(
        "speed-race",
        vec![fast_agent.clone(), slow_agent.clone()],
    );

    let state = State::new();
    let start = std::time::Instant::now();
    let result = race.run(&state).await.unwrap();
    let elapsed = start.elapsed();

    println!("Race result: '{}'", result);
    println!("Completed in {:?} (fast agent won, slow cancelled)", elapsed);
    assert!(elapsed < Duration::from_millis(150), "Fast agent should win");

    // ── 2. Race with multiple providers (model selection) ──
    println!("\n--- 2. Multi-Provider Race ---\n");

    let provider_a: Arc<dyn BaseLlm> = Arc::new(DelayLlm {
        name: "provider-a".into(),
        delay: Duration::from_millis(80),
        response: "Provider A: Comprehensive analysis of the market trends".into(),
    });

    let provider_b: Arc<dyn BaseLlm> = Arc::new(DelayLlm {
        name: "provider-b".into(),
        delay: Duration::from_millis(60),
        response: "Provider B: Quick market overview with key metrics".into(),
    });

    let provider_c: Arc<dyn BaseLlm> = Arc::new(DelayLlm {
        name: "provider-c".into(),
        delay: Duration::from_millis(100),
        response: "Provider C: Detailed market report with forecasts".into(),
    });

    let agent_a = AgentBuilder::new("provider-a").instruction("Analyze").build(provider_a);
    let agent_b = AgentBuilder::new("provider-b").instruction("Analyze").build(provider_b);
    let agent_c = AgentBuilder::new("provider-c").instruction("Analyze").build(provider_c);

    let multi_race = RaceTextAgent::new(
        "provider-race",
        vec![agent_a, agent_b, agent_c],
    );

    let result = multi_race.run(&state).await.unwrap();
    println!("Multi-provider race winner: '{}'", result);

    // ── 3. Basic timeout ──
    println!("\n--- 3. Basic Timeout ---\n");

    let timeout_agent = TimeoutTextAgent::new(
        "fast-with-timeout",
        fast_agent.clone(),
        Duration::from_millis(500),
    );

    let result = timeout_agent.run(&state).await;
    println!("Fast agent with 500ms timeout: {:?}", result.is_ok());

    let tight_timeout = TimeoutTextAgent::new(
        "slow-with-tight-timeout",
        slow_agent.clone(),
        Duration::from_millis(50), // Tighter than the slow agent's 200ms
    );

    let result = tight_timeout.run(&state).await;
    println!("Slow agent with 50ms timeout: {:?} (expected timeout)", result.is_err());
    if let Err(ref e) = result {
        println!("  Error: {}", e);
    }

    // ── 4. Timeout + Fallback ──
    println!("\n--- 4. Timeout + Fallback ---\n");

    // Wrap the slow agent with a timeout, fall back to the fast agent
    let slow_with_timeout: Arc<dyn TextAgent> = Arc::new(TimeoutTextAgent::new(
        "slow-limited",
        slow_agent.clone(),
        Duration::from_millis(100), // Will timeout
    ));

    let with_fallback = FallbackTextAgent::new(
        "timeout-fallback",
        vec![slow_with_timeout, fast_agent.clone()],
    );

    let result = with_fallback.run(&state).await.unwrap();
    println!("Timeout+fallback result: '{}'", result);
    println!("  Slow agent timed out, fast agent took over");

    // ── 5. Race + Timeout for production resilience ──
    println!("\n--- 5. Production Resilience Pattern ---\n");

    // In production: race multiple providers, each with individual timeouts
    let create_provider = |name: &str, delay_ms: u64, response: &str, timeout_ms: u64|
        -> Arc<dyn TextAgent>
    {
        let llm: Arc<dyn BaseLlm> = Arc::new(DelayLlm {
            name: name.into(),
            delay: Duration::from_millis(delay_ms),
            response: response.into(),
        });
        let agent = AgentBuilder::new(name)
            .instruction("Respond")
            .build(llm);
        Arc::new(TimeoutTextAgent::new(
            format!("{}-timeout", name),
            agent,
            Duration::from_millis(timeout_ms),
        ))
    };

    // Provider 1: fast but may be down (simulated by timeout)
    let p1 = create_provider("primary", 40, "Primary: fast response", 200);
    // Provider 2: medium speed, reliable
    let p2 = create_provider("secondary", 80, "Secondary: reliable response", 200);
    // Provider 3: slow but thorough
    let p3 = create_provider("tertiary", 150, "Tertiary: thorough response", 200);

    let resilient = RaceTextAgent::new("resilient-race", vec![p1, p2, p3]);

    let start = std::time::Instant::now();
    let result = resilient.run(&state).await.unwrap();
    let elapsed = start.elapsed();
    println!("Resilient race: '{}' in {:?}", result, elapsed);

    // ── 6. Cascading timeouts (increasing patience) ──
    println!("\n--- 6. Cascading Timeouts ---\n");

    // Try fast first (tight timeout), then medium (more time), then slow (generous)
    let fast_try: Arc<dyn TextAgent> = Arc::new(TimeoutTextAgent::new(
        "fast-try",
        AgentBuilder::new("fast").instruction("Quick").build(fast_llm.clone()),
        Duration::from_millis(30),
    ));

    let medium_llm: Arc<dyn BaseLlm> = Arc::new(DelayLlm {
        name: "medium".into(),
        delay: Duration::from_millis(100),
        response: "Medium-quality response".into(),
    });
    let medium_try: Arc<dyn TextAgent> = Arc::new(TimeoutTextAgent::new(
        "medium-try",
        AgentBuilder::new("medium").instruction("Moderate").build(medium_llm),
        Duration::from_millis(200),
    ));

    let slow_try: Arc<dyn TextAgent> = Arc::new(TimeoutTextAgent::new(
        "slow-try",
        AgentBuilder::new("slow").instruction("Thorough").build(slow_llm.clone()),
        Duration::from_millis(500),
    ));

    let cascade = FallbackTextAgent::new(
        "cascading-timeout",
        vec![fast_try, medium_try, slow_try],
    );

    let result = cascade.run(&state).await.unwrap();
    println!("Cascading timeout result: '{}'", result);

    // ── 7. Race with observation (Tap) ──
    println!("\n--- 7. Race with Observation ---\n");

    // Use TapTextAgent to observe the race winner
    let observe_winner = TapTextAgent::new("race-observer", |state: &State| {
        if let Some(output) = state.get::<String>("output") {
            println!("  [OBSERVER] Race winner produced {} chars", output.len());
        }
    });

    // Sequential: race >> observe
    let observed_race = SequentialTextAgent::new(
        "observed-race",
        vec![
            Arc::new(RaceTextAgent::new(
                "inner-race",
                vec![fast_agent.clone(), slow_agent.clone()],
            )) as Arc<dyn TextAgent>,
            Arc::new(observe_winner) as Arc<dyn TextAgent>,
        ],
    );

    observed_race.run(&state).await.unwrap();

    // ── 8. Operator-based composition with timeout ──
    println!("\n--- 8. Operator Composition ---\n");

    // AgentBuilder operators create Composable trees
    // Timeout wraps at the TextAgent level after compilation
    let research = AgentBuilder::new("research")
        .instruction("Research the topic")
        .temperature(0.3);

    let analyze = AgentBuilder::new("analyze")
        .instruction("Analyze findings")
        .temperature(0.2);

    // Pipeline: research >> analyze (sequential)
    let pipeline = research.clone() >> analyze.clone();

    // Fallback: try pipeline, fall back to quick response
    let quick = AgentBuilder::new("quick")
        .instruction("Provide a quick answer");

    let robust = pipeline / quick;

    println!("Composed: (research >> analyze) / quick");
    match &robust {
        Composable::Fallback(f) => println!("  Fallback with {} candidates", f.candidates.len()),
        _ => {}
    }

    println!("\nRace/Timeout pipeline example completed successfully!");
}
