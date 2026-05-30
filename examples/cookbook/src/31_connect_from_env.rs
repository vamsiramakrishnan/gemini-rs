//! Cookbook #31 — Zero-ceremony connection with `connect_from_env`
//!
//! Connecting used to mean hand-rolling auth: detect the platform, read a
//! pile of environment variables, and shell out to `gcloud` for a Vertex
//! token. `Live::connect_from_env()` (and the lower-level
//! `ApiEndpoint::from_env()`) collapse all of that into one call.
//!
//! This example resolves an endpoint from the current environment and
//! reports what it found — it does NOT open a connection, so it runs
//! without credentials.

use gemini_adk_fluent_rs::prelude::*;

fn main() {
    println!("=== Cookbook #31: connect_from_env ===\n");

    // ── Before: the ceremony every app used to copy ──
    println!("--- Before (≈30 lines, per app) ---\n");
    println!("    let use_vertex = std::env::var(\"GOOGLE_GENAI_USE_VERTEXAI\")...;");
    println!("    let auth = if use_vertex {{");
    println!("        let project = std::env::var(\"GOOGLE_CLOUD_PROJECT\").expect(...);");
    println!("        let location = std::env::var(\"GOOGLE_CLOUD_LOCATION\")...;");
    println!("        let token = String::from_utf8(");
    println!("            std::process::Command::new(\"gcloud\")");
    println!("                .args([\"auth\", \"print-access-token\"]).output()");
    println!("                .expect(\"gcloud CLI required\").stdout).unwrap()...;");
    println!("        SessionConfig::from_vertex(project, location, token)");
    println!("    }} else {{ SessionConfig::new(std::env::var(\"GEMINI_API_KEY\")?) }};");

    // ── After: one call ──
    println!("\n--- After (1 line) ---\n");
    println!("    let handle = Live::builder()");
    println!("        .model(GeminiModel::Gemini2_0FlashLive)");
    println!("        .voice(Voice::Kore)");
    println!("        .connect_from_env()   // ← resolves platform, creds, and token");
    println!("        .await?;");

    // ── What it reads ──
    println!("\n--- Resolution rules ---\n");
    println!("  GOOGLE_GENAI_USE_VERTEXAI=true → Vertex AI:");
    println!("      GOOGLE_CLOUD_PROJECT   (required)");
    println!("      GOOGLE_CLOUD_LOCATION  (default: us-central1)");
    println!("      GOOGLE_ACCESS_TOKEN    (else: `gcloud auth print-access-token`)");
    println!("  otherwise → Google AI:");
    println!("      GEMINI_API_KEY | GOOGLE_GENAI_API_KEY | GOOGLE_API_KEY");

    // ── Live resolution against the current environment ──
    println!("\n--- Resolving from YOUR environment ---\n");
    match ApiEndpoint::from_env() {
        Ok(ApiEndpoint::GoogleAI { .. }) | Ok(ApiEndpoint::GoogleAIToken { .. }) => {
            println!("  ✓ Resolved Google AI endpoint (API key found).");
            println!("    `connect_from_env()` would connect to Google AI Studio.");
        }
        Ok(ApiEndpoint::VertexAI(cfg)) => {
            println!(
                "  ✓ Resolved Vertex AI endpoint (project={}, location={}).",
                cfg.project, cfg.location
            );
            println!("    `connect_from_env()` would connect to Vertex AI.");
        }
        Err(e) => {
            println!("  ✗ No credentials in the environment: {e}");
            println!("    Set GEMINI_API_KEY (Google AI) or GOOGLE_GENAI_USE_VERTEXAI=true");
            println!("    with GOOGLE_CLOUD_PROJECT (Vertex AI) and re-run.");
        }
    }

    println!("\nconnect_from_env example completed successfully!");
}
