//! Call lifecycle example — inbound call with hold, resume, and hangup.
//!
//! Demonstrates the CallSession API for speech-to-speech telephony applications.
//! Uses mock audio source/sink since real audio I/O is transport-specific.

use gemini_live_rs::prelude::*;
use std::future::Future;
use std::pin::Pin;

// ---------------------------------------------------------------------------
// Mock audio I/O (replace with WebSocket, RTP, or microphone in production)
// ---------------------------------------------------------------------------

struct MockAudioSource;

impl AudioSource for MockAudioSource {
    fn read_frame(&mut self) -> Pin<Box<dyn Future<Output = Option<Vec<i16>>> + Send>> {
        Box::pin(async {
            // Simulate audio input at 30ms intervals
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            Some(vec![0i16; 480]) // 30ms of silence at 16kHz
        })
    }
    fn sample_rate(&self) -> u32 {
        16_000
    }
}

struct MockAudioSink;

impl AudioSink for MockAudioSink {
    fn write_frame(
        &mut self,
        _samples: &[i16],
    ) -> Pin<Box<dyn Future<Output = Result<(), Box<dyn std::error::Error + Send>>> + Send>> {
        Box::pin(async { Ok(()) })
    }
    fn sample_rate(&self) -> u32 {
        24_000
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GEMINI_API_KEY")
        .expect("Set GEMINI_API_KEY environment variable");

    // Build agent with tools and callbacks
    let agent = GeminiAgent::builder()
        .api_key(api_key)
        .voice(Voice::Kore)
        .system_instruction("You are a customer service agent for TechCorp.")
        .on_text(|t| async move { print!("{t}") })
        .on_turn_complete(|_| async move { println!("\n---") })
        .build()
        .await?;

    println!("Agent ready. Accepting inbound call...\n");

    // Accept an inbound call
    let mut call = CallSession::inbound(
        agent.session().clone(),
        agent.pipeline_config.clone(),
        Box::new(MockAudioSource),
        Box::new(MockAudioSink),
    )
    .await?;

    println!("Call active. Phase: {:?}\n", call.phase());

    // Send a greeting
    call.send_text("Welcome to TechCorp! How can I help you today?")
        .await?;

    // Simulate some conversation time
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // Put the call on hold
    println!("\n[Putting call on hold...]");
    call.hold().await?;
    println!("Call phase: {:?}", call.phase());

    // Simulate hold time
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Resume the call with fresh audio I/O
    println!("[Resuming call...]");
    call.resume(
        PipelineConfig::default(),
        Box::new(MockAudioSource),
        Box::new(MockAudioSink),
    )
    .await?;
    println!("Call phase: {:?}\n", call.phase());

    call.send_text("Sorry for the wait! I've found the information you need.")
        .await?;

    // Wait a bit then hang up
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let metrics = call.hangup().await?;
    println!("\n=== Call Summary ===");
    println!("Duration: {:?}", metrics.duration());
    println!("Turns: {}", metrics.turn_count);
    println!("Interruptions: {}", metrics.interruption_count);
    println!("Tool calls: {}", metrics.tool_call_count);
    println!("Hold time: {:?}", metrics.hold_duration);
    println!("Tools used: {:?}", metrics.tools_used);

    Ok(())
}
