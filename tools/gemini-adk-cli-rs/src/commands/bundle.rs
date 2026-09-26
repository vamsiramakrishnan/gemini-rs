//! `adk bundle` — push session specs to a bundle store, list their versions,
//! label them and fetch them back.
//!
//! The store is `--store`, else `ADK_BUNDLES`, else `./bundles`: a directory
//! path or `gs://bucket/prefix`.

use std::fs;
use std::sync::Arc;

use gemini_adk_fluent_rs::spec::{BundleRef, BundleStore, SessionSpec, open_store};
use serde_json::Value;

type CliResult = Result<(), Box<dyn std::error::Error>>;

fn store(uri: Option<&str>) -> Result<Arc<dyn BundleStore>, Box<dyn std::error::Error>> {
    let uri = uri
        .map(str::to_string)
        .or_else(|| std::env::var("ADK_BUNDLES").ok())
        .unwrap_or_else(|| "bundles".to_string());
    Ok(open_store(&uri)?)
}

/// A bundle name from a spec name: lowercase, with `-` for anything else.
fn name_from(spec: &SessionSpec) -> String {
    let name: String = spec
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    name.trim_matches('-').to_string()
}

/// `adk bundle push <spec> [--name] [-m] [--label]…`
pub async fn push(
    spec_path: &str,
    name: Option<&str>,
    message: Option<&str>,
    labels: &[String],
    uri: Option<&str>,
) -> CliResult {
    let raw = fs::read_to_string(spec_path).map_err(|e| format!("{spec_path}: {e}"))?;
    let value: Value = serde_json::from_str(&raw).map_err(|e| format!("{spec_path}: {e}"))?;
    let spec = SessionSpec::from_value(value)?;
    let name = match name {
        Some(name) => name.to_string(),
        None => name_from(&spec),
    };
    if name.is_empty() {
        return Err("the spec has no name: pass --name".into());
    }
    let store = store(uri)?;
    let version = store.push(&name, &spec, message).await?;
    println!("{name}@{}  {}", version.version, version.created_at);
    for label in labels {
        store.set_label(&name, label, &version.version).await?;
        println!("{name}:{label} -> {}", version.version);
    }
    Ok(())
}

/// `adk bundle list [name]` — every bundle, or one bundle's versions.
pub async fn list(name: Option<&str>, uri: Option<&str>) -> CliResult {
    let store = store(uri)?;
    let Some(name) = name else {
        for name in store.names().await? {
            let labels = store.labels(&name).await?;
            let labels: Vec<String> = labels.keys().map(|l| format!(":{l}")).collect();
            println!("{name}  {}", labels.join(" "));
        }
        return Ok(());
    };
    let labels = store.labels(name).await?;
    for version in store.versions(name).await? {
        let pointing: Vec<String> = labels
            .iter()
            .filter(|(_, v)| **v == version.version)
            .map(|(l, _)| format!(":{l}"))
            .collect();
        println!(
            "{}  {}  {}  {}",
            version.version,
            version.created_at,
            pointing.join(" "),
            version.message.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

/// `adk bundle get <name[@version|:label]> [--out file]`
pub async fn get(reference: &str, out: Option<&str>, uri: Option<&str>) -> CliResult {
    let (name, reference) = BundleRef::parse(reference);
    let (version, spec) = store(uri)?.get(&name, &reference).await?;
    let mut json = serde_json::to_string_pretty(&spec)?;
    json.push('\n');
    match out {
        Some(path) => {
            fs::write(path, json)?;
            eprintln!("{name}@{} -> {path}", version.version);
        }
        None => print!("{json}"),
    }
    Ok(())
}

/// `adk bundle label <name> <label> <version>`
pub async fn label(name: &str, label: &str, version: &str, uri: Option<&str>) -> CliResult {
    let version = store(uri)?.set_label(name, label, version).await?;
    println!("{name}:{label} -> {version}");
    Ok(())
}
