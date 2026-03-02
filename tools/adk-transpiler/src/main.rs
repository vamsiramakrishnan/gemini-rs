mod codegen;
mod diff;
mod reader;
mod schema;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "adk-transpiler")]
#[command(about = "Extracts agent definitions from ADK-JS TypeScript source for Rust codegen")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Read ADK-JS TypeScript source files and extract agent/tool definitions
    Read {
        /// Path to the ADK-JS source directory (e.g. /tmp/adk-js/core/src/agents/)
        #[arg(short, long)]
        source: PathBuf,

        /// Output JSON file path
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Diff two schema JSON files and report changes
    Diff {
        /// Path to the old schema JSON file
        #[arg(long)]
        old: PathBuf,

        /// Path to the new schema JSON file
        #[arg(long)]
        new: PathBuf,
    },
    /// Generate Rust code from an AdkSchema JSON file (standalone, with placeholder traits)
    Generate {
        /// Path to the input AdkSchema JSON file
        #[arg(short, long)]
        schema: PathBuf,

        /// Output Rust source file path
        #[arg(short, long)]
        output: PathBuf,
    },
    /// One-shot: read ADK-JS source and generate compilable Rust code targeting gemini-live-runtime
    Transpile {
        /// Path to the ADK-JS source directory (e.g. /tmp/adk-js/core/src/agents/)
        #[arg(short, long)]
        source: PathBuf,

        /// Output Rust source file path (e.g. crates/gemini-live-runtime/src/agents/generated.rs)
        #[arg(short, long)]
        output: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Read { source, output } => {
            run_read(&source, &output);
        }
        Commands::Diff { old, new } => {
            run_diff(&old, &new);
        }
        Commands::Generate { schema, output } => {
            run_generate(&schema, &output);
        }
        Commands::Transpile { source, output } => {
            run_transpile(&source, &output);
        }
    }
}

fn run_read(source: &Path, output: &Path) {
    eprintln!("Reading TypeScript source from: {}", source.display());

    match reader::read_source_dir(source) {
        Ok(schema) => {
            eprintln!(
                "Extracted {} agents, {} tools",
                schema.agents.len(),
                schema.tools.len()
            );

            let json = serde_json::to_string_pretty(&schema).expect("Failed to serialize schema");

            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent).ok();
            }

            std::fs::write(output, &json)
                .unwrap_or_else(|e| panic!("Failed to write {}: {}", output.display(), e));

            eprintln!("Schema written to: {}", output.display());

            // Print summary
            println!("=== ADK Schema Summary ===");
            println!("Framework: {}", schema.source.framework);
            println!("Source: {}", schema.source.source_dir);
            println!("Extracted at: {}", schema.source.extracted_at);
            println!();
            println!("Agents ({}):", schema.agents.len());
            for agent in &schema.agents {
                println!(
                    "  - {} ({:?}) [{} fields, {} callbacks]{}",
                    agent.name,
                    agent.kind,
                    agent.fields.len(),
                    agent.callbacks.len(),
                    agent
                        .extends
                        .as_ref()
                        .map(|e| format!(" extends {}", e))
                        .unwrap_or_default()
                );
            }
            println!();
            println!("Tools ({}):", schema.tools.len());
            for tool in &schema.tools {
                println!(
                    "  - {} [{} fields]{}",
                    tool.name,
                    tool.fields.len(),
                    tool.extends
                        .as_ref()
                        .map(|e| format!(" extends {}", e))
                        .unwrap_or_default()
                );
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

fn run_diff(old_path: &Path, new_path: &Path) {
    let old_json = std::fs::read_to_string(old_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", old_path.display(), e));
    let new_json = std::fs::read_to_string(new_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", new_path.display(), e));

    let old_schema: schema::AdkSchema = serde_json::from_str(&old_json)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {}", old_path.display(), e));
    let new_schema: schema::AdkSchema = serde_json::from_str(&new_json)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {}", new_path.display(), e));

    let result = diff::diff_schemas(&old_schema, &new_schema);
    print!("{}", result);

    if result.is_empty() {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}

fn run_generate(schema_path: &Path, output_path: &Path) {
    eprintln!("Reading schema from: {}", schema_path.display());

    let json = std::fs::read_to_string(schema_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", schema_path.display(), e));

    let schema: schema::AdkSchema = serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {}", schema_path.display(), e));

    eprintln!(
        "Schema contains {} agents, {} tools",
        schema.agents.len(),
        schema.tools.len()
    );

    let rust_code = codegen::generate(&schema);

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    std::fs::write(output_path, &rust_code)
        .unwrap_or_else(|e| panic!("Failed to write {}: {}", output_path.display(), e));

    eprintln!("Generated Rust code written to: {}", output_path.display());

    // Print summary
    println!("=== Codegen Summary ===");
    println!("Source framework: {}", schema.source.framework);
    println!("Agents generated: {}", schema.agents.len());
    println!("Tools generated: {}", schema.tools.len());
    println!(
        "Output size: {} bytes",
        rust_code.len()
    );
}

fn run_transpile(source: &Path, output: &Path) {
    eprintln!("Transpiling ADK-JS source from: {}", source.display());

    // Step 1: Read TypeScript source
    let schema = match reader::read_source_dir(source) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading source: {}", e);
            std::process::exit(1);
        }
    };

    eprintln!(
        "Extracted {} agents, {} tools",
        schema.agents.len(),
        schema.tools.len()
    );

    // Step 2: Generate compilable Rust code
    let rust_code = codegen::generate_compilable(&schema);

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    std::fs::write(output, &rust_code)
        .unwrap_or_else(|e| panic!("Failed to write {}: {}", output.display(), e));

    eprintln!("Compilable Rust code written to: {}", output.display());

    // Print summary
    println!("=== Transpile Summary ===");
    println!("Source: {}", source.display());
    println!("Output: {}", output.display());
    println!("Framework: {}", schema.source.framework);
    println!("Agents: {}", schema.agents.len());
    println!("Tools: {}", schema.tools.len());
    println!("Output size: {} bytes", rust_code.len());
    println!("Target: gemini-live-runtime (compilable)");
}
