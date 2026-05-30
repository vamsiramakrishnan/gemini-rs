//! Cookbook #32 — The Live session callback catalog
//!
//! Every observability hook a Live session offers, grouped by the lane it
//! runs on. This example *builds* the session (it does not connect, so no
//! credentials are needed) to demonstrate the full callback surface and the
//! fast-lane vs control-lane distinction.
//!
//! - **Fast lane** (sync, must be <1ms — no allocations/locks/async):
//!   `on_audio`, `on_text`, `on_text_complete`, `on_input_transcript`,
//!   `on_output_transcript`, `on_thought`, `on_vad_start`, `on_vad_end`,
//!   `on_phase`, `on_usage`.
//! - **Control lane** (async, may block; many have `_concurrent` variants):
//!   `on_interrupted`, `on_tool_call`, `on_turn_complete`, `on_connected`,
//!   `on_disconnected`, `on_go_away`, `on_error`, `on_tool_cancelled`,
//!   `on_generation_complete`, `on_resumed`.

use gemini_adk_fluent_rs::prelude::*;

fn main() {
    println!("=== Cookbook #32: Live Callback Catalog ===\n");

    // Build a session wired to every callback. We never connect, so this runs
    // without credentials — it exists to show the surface and that it compiles.
    let _live = Live::builder()
        .model(GeminiModel::Gemini2_0FlashLive)
        .voice(Voice::Kore)
        .instruction("You are a helpful voice assistant.")
        // ── Fast lane: sync, <1ms, no async/locks ──
        .on_audio(|pcm| {
            // e.g. playback_tx.try_send(pcm.clone()).ok();
            let _ = pcm;
        })
        .on_text(|delta| print!("{delta}"))
        .on_text_complete(|full| println!("\n[text complete] {} chars", full.len()))
        .on_input_transcript(|text, is_final| {
            // Partial results stream with is_final=false; the finalized
            // utterance arrives once with is_final=true at the turn boundary.
            if is_final {
                println!("[user] {text}");
            }
        })
        .on_output_transcript(|text, is_final| {
            if is_final {
                println!("[model] {text}");
            }
        })
        .on_thought(|t| println!("[thought] {t}"))
        .on_vad_start(|| println!("[vad] speech started"))
        .on_vad_end(|| println!("[vad] speech ended"))
        .on_phase(|phase| println!("[phase] {phase:?}"))
        .on_usage(|usage| println!("[usage] {usage:?}"))
        // ── Control lane: async, may block; _concurrent spawns detached ──
        .on_interrupted(|| async { println!("[interrupted]") })
        .on_turn_complete(|| async { println!("[turn complete]") })
        .on_generation_complete(|| async {
            // Fires when the model finishes its FULL intended response, before
            // any interruption truncation — distinct from on_turn_complete.
            println!("[generation complete]");
        })
        .on_tool_cancelled(|ids| async move {
            println!("[tool cancelled] {ids:?}");
        })
        .on_resumed(|| async {
            // Fires when a persisted session resumes.
            println!("[resumed]");
        })
        .on_connected(|_writer| async { println!("[connected]") })
        .on_disconnected(|reason| async move { println!("[disconnected] {reason:?}") })
        .on_go_away(|d| async move { println!("[go away] {d:?} left") })
        .on_error(|e| async move { eprintln!("[error] {e}") })
        // Fire-and-forget variant: detached task, never blocks the loop.
        .on_turn_complete_concurrent(|| async { /* analytics ping */ });

    println!("Live session wired to the full callback surface (not connected).");
    println!("\nSee CLAUDE.md › 'Live Session Callbacks' for the lane rules.");
    println!("callback catalog example completed successfully!");
}
