use std::env;
use std::net::TcpListener;
use std::process::Command;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    println!("\n  ADK Doctor — Environment Check\n");

    let mut issues: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

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

    // ── Auth ─────────────────────────────────────────────────────────
    print!("  Google AI API key ......... ");
    let has_key = ["GOOGLE_GENAI_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY"]
        .iter()
        .find(|k| env::var(k).is_ok());
    match has_key {
        Some(key_name) => println!("set ({})", key_name),
        None => println!("not set"),
    }

    // ── Vertex AI ────────────────────────────────────────────────────
    print!("  Vertex AI project ......... ");
    let vertex_project = env::var("GOOGLE_CLOUD_PROJECT")
        .or_else(|_| env::var("GOOGLE_PROJECT_ID"))
        .ok();
    match vertex_project.as_deref() {
        Some(project) => {
            let loc = env::var("GOOGLE_CLOUD_LOCATION").unwrap_or_else(|_| "us-central1".into());
            println!("{} ({})", project, loc);
        }
        None => println!("not configured (optional)"),
    }
    if has_key.is_none() && vertex_project.is_none() {
        issues.push("Configure either a Google AI API key or Vertex AI project credentials".into());
    }

    // ── gcloud CLI (optional) ────────────────────────────────────────
    print!("  gcloud CLI ................ ");
    match Command::new("gcloud").arg("version").output() {
        Ok(out) if out.status.success() => println!("installed"),
        _ => {
            println!("not found (optional — needed for Vertex AI & deploy)");
            if vertex_project.is_some() {
                warnings.push("Vertex AI project is configured but gcloud is not installed".into());
            }
        }
    }

    // ── GitHub CLI ───────────────────────────────────────────────────
    print!("  gh auth ................... ");
    match Command::new("gh").arg("auth").arg("status").output() {
        Ok(out) if out.status.success() => println!("authenticated"),
        Ok(_) => {
            println!("not authenticated");
            warnings.push("Run `gh auth login` before using GitHub merge/release workflows".into());
        }
        Err(_) => println!("gh not found (optional)"),
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

    // ── Native deps ──────────────────────────────────────────────────
    check_pkg_config("OpenSSL", "openssl", &mut warnings);
    check_pkg_config("ALSA", "alsa", &mut warnings);

    // ── Node (for web UI checks) ─────────────────────────────────────
    print!("  node ...................... ");
    match Command::new("node").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout);
            println!("{}", ver.trim());
        }
        _ => println!("not found (optional — needed for frontend validation)"),
    }

    // ── Dev server ports ─────────────────────────────────────────────
    check_port(8000, "adk web default", &mut warnings);
    check_port(25125, "examples web UI", &mut warnings);

    // ── .env file (check cwd) ────────────────────────────────────────
    print!("  .env file (cwd) .......... ");
    if std::path::Path::new(".env").exists() {
        println!("found");
    } else {
        println!("not found (optional — env vars can be set directly)");
    }

    // ── Summary ──────────────────────────────────────────────────────
    println!();
    if !warnings.is_empty() {
        println!("  {} warning(s):\n", warnings.len());
        for warning in &warnings {
            println!("    -> {}", warning);
        }
        println!();
    }
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

fn check_pkg_config(label: &str, package: &str, warnings: &mut Vec<String>) {
    print!("  {:<27}", format!("{label} pkg-config ...."));
    match Command::new("pkg-config")
        .arg("--exists")
        .arg(package)
        .status()
    {
        Ok(status) if status.success() => println!("found"),
        Ok(_) => {
            println!("not found");
            warnings.push(format!(
                "{label} development package was not found by pkg-config"
            ));
        }
        Err(_) => {
            println!("pkg-config not found");
            push_warning_once(
                warnings,
                "Install pkg-config to diagnose native library linkage".into(),
            );
        }
    }
}

fn check_port(port: u16, label: &str, warnings: &mut Vec<String>) {
    print!("  {:<27}", format!("{label} ...."));
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(_) => println!("available"),
        Err(_) => {
            println!("in use");
            warnings.push(format!(
                "Port {port} is already in use; choose a free port or stop the existing server"
            ));
        }
    }
}

fn push_warning_once(warnings: &mut Vec<String>, warning: String) {
    if !warnings.iter().any(|existing| existing == &warning) {
        warnings.push(warning);
    }
}
