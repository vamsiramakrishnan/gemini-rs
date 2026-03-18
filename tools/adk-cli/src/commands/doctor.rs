use std::env;
use std::process::Command;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n  ADK Doctor — Environment Check\n");

    let mut issues: Vec<String> = Vec::new();

    // ── Rust toolchain ───────────────────────────────────────────────
    print!("  Rust toolchain ............ ");
    match Command::new("rustc").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout);
            println!("{}", ver.trim());
        }
        _ => {
            println!("NOT FOUND");
            issues.push("Install Rust: https://rustup.rs".into());
        }
    }

    // ── Google AI API key ────────────────────────────────────────────
    print!("  Google AI API key ......... ");
    let has_key = ["GOOGLE_GENAI_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY"]
        .iter()
        .find(|k| env::var(k).is_ok());
    match has_key {
        Some(key_name) => println!("set ({})", key_name),
        None => {
            println!("NOT SET");
            issues.push(
                "Set one of: GOOGLE_GENAI_API_KEY, GEMINI_API_KEY, or GOOGLE_API_KEY".into(),
            );
        }
    }

    // ── Vertex AI (optional) ─────────────────────────────────────────
    print!("  Vertex AI project ......... ");
    match env::var("GOOGLE_CLOUD_PROJECT") {
        Ok(project) => {
            let loc = env::var("GOOGLE_CLOUD_LOCATION").unwrap_or_else(|_| "us-central1".into());
            println!("{} ({})", project, loc);
        }
        Err(_) => println!("not configured (optional)"),
    }

    // ── gcloud CLI (optional) ────────────────────────────────────────
    print!("  gcloud CLI ................ ");
    match Command::new("gcloud").arg("version").output() {
        Ok(out) if out.status.success() => println!("installed"),
        _ => println!("not found (optional — needed for Vertex AI & deploy)"),
    }

    // ── cargo (sanity) ───────────────────────────────────────────────
    print!("  cargo ..................... ");
    match Command::new("cargo").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout);
            println!("{}", ver.trim());
        }
        _ => {
            println!("NOT FOUND");
            issues.push("cargo not found — is Rust installed correctly?".into());
        }
    }

    // ── .env file (check cwd) ────────────────────────────────────────
    print!("  .env file (cwd) .......... ");
    if std::path::Path::new(".env").exists() {
        println!("found");
    } else {
        println!("not found (optional — env vars can be set directly)");
    }

    // ── Summary ──────────────────────────────────────────────────────
    println!();
    if issues.is_empty() {
        println!("  All checks passed. Ready to build agents!\n");
        println!("  Quick start:");
        println!("    adk create my-agent");
        println!("    cd my-agent");
        println!("    adk run .\n");
    } else {
        println!("  {} issue(s) found:\n", issues.len());
        for issue in &issues {
            println!("    -> {}", issue);
        }
        println!();
    }

    Ok(())
}
