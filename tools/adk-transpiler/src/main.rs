mod codegen;
mod diff;
mod genai_reader;
mod reader;
mod schema;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "adk-transpiler")]
#[command(about = "Transpiles ADK-JS and js-genai TypeScript to Rust targeting rs-genai")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Read ADK-JS TypeScript source files and extract agent/tool definitions
    Read {
        /// Path to the ADK-JS source directory (e.g. /tmp/adk-js/core/src/)
        #[arg(short, long)]
        source: PathBuf,

        /// Output JSON file path
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Read js-genai TypeScript source and extract SDK type definitions
    ReadGenai {
        /// Path to the js-genai source directory (e.g. /tmp/js-genai/src/)
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
    /// One-shot: read ADK-JS source and generate compilable Rust code targeting rs-adk
    Transpile {
        /// Path to the ADK-JS source directory (e.g. /tmp/adk-js/core/src/)
        #[arg(short, long)]
        source: PathBuf,

        /// Output Rust source file path (e.g. crates/rs-adk/src/agents/generated.rs)
        #[arg(short, long)]
        output: PathBuf,

        /// Optional: path to js-genai source for precise type resolution
        #[arg(short, long)]
        genai_source: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Read { source, output } => {
            run_read(&source, &output);
        }
        Commands::ReadGenai { source, output } => {
            run_read_genai(&source, &output);
        }
        Commands::Diff { old, new } => {
            run_diff(&old, &new);
        }
        Commands::Generate { schema, output } => {
            run_generate(&schema, &output);
        }
        Commands::Transpile {
            source,
            output,
            genai_source,
        } => {
            run_transpile(&source, &output, genai_source.as_deref());
        }
    }
}

fn run_read(source: &Path, output: &Path) {
    eprintln!("Reading TypeScript source from: {}", source.display());

    match reader::read_source_dir(source) {
        Ok(schema) => {
            eprintln!(
                "Extracted {} agents, {} tools, {} types",
                schema.agents.len(),
                schema.tools.len(),
                schema.types.len()
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
            println!();
            println!("Types ({}):", schema.types.len());
            for type_def in &schema.types {
                if type_def.is_enum {
                    println!(
                        "  - {} [enum, {} variants] (module: {})",
                        type_def.name,
                        type_def.variants.len(),
                        type_def.module
                    );
                } else {
                    println!(
                        "  - {} [{} fields] (module: {}){}",
                        type_def.name,
                        type_def.fields.len(),
                        type_def.module,
                        type_def
                            .extends
                            .as_ref()
                            .map(|e| format!(" extends {}", e))
                            .unwrap_or_default()
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

fn run_read_genai(source: &Path, output: &Path) {
    eprintln!(
        "Reading js-genai TypeScript source from: {}",
        source.display()
    );

    match genai_reader::read_genai_source(source) {
        Ok(schema) => {
            let with_wire = schema.types.iter().filter(|t| t.has_wire_equivalent).count();
            let total_types = schema.types.len();
            let with_wire_enums = schema.enums.iter().filter(|e| e.has_wire_equivalent).count();
            let total_enums = schema.enums.len();

            eprintln!(
                "Extracted {} types ({} with wire equiv), {} enums ({} with wire equiv), {} aliases, {} helpers",
                total_types, with_wire,
                total_enums, with_wire_enums,
                schema.type_aliases.len(),
                schema.helpers.len()
            );

            let json =
                serde_json::to_string_pretty(&schema).expect("Failed to serialize schema");

            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent).ok();
            }

            std::fs::write(output, &json)
                .unwrap_or_else(|e| panic!("Failed to write {}: {}", output.display(), e));

            eprintln!("Schema written to: {}", output.display());

            // Print summary
            println!("=== js-genai Schema Summary ===");
            println!("Source: {}", schema.source.source_dir);
            println!();
            println!(
                "Types: {} total, {} with wire equivalent",
                total_types, with_wire
            );
            println!();

            // Show wire mappings
            println!("Wire Mappings:");
            for t in &schema.types {
                if let Some(ref wire) = t.wire_type {
                    println!("  {} → {}", t.name, wire);
                }
            }
            for e in &schema.enums {
                if let Some(ref wire) = e.wire_type {
                    println!("  {} → {}", e.name, wire);
                }
            }

            println!();
            println!("Types without wire equivalent ({}):", total_types - with_wire);
            for t in &schema.types {
                if !t.has_wire_equivalent {
                    println!(
                        "  - {} ({:?}) [{} fields]",
                        t.name,
                        t.category,
                        t.fields.len()
                    );
                }
            }

            println!();
            println!("Helpers:");
            for h in &schema.helpers {
                let mapping = h
                    .wire_equivalent
                    .as_ref()
                    .map(|w| format!(" → {}", w))
                    .unwrap_or_default();
                println!("  - {}{}", h.name, mapping);
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
        "Schema contains {} agents, {} tools, {} types",
        schema.agents.len(),
        schema.tools.len(),
        schema.types.len()
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
    println!("Types generated: {}", schema.types.len());
    println!("Output size: {} bytes", rust_code.len());
}

fn run_transpile(source: &Path, output: &Path, genai_source: Option<&Path>) {
    eprintln!("Transpiling ADK-JS source from: {}", source.display());

    // Step 1: Optionally read js-genai types for precise resolution
    let genai_lookup = if let Some(genai_path) = genai_source {
        eprintln!(
            "Reading js-genai types from: {}",
            genai_path.display()
        );
        match genai_reader::read_genai_source(genai_path) {
            Ok(genai_schema) => {
                let lookup = genai_reader::build_type_lookup(&genai_schema);
                eprintln!(
                    "  {} types, {} enums, {} wire mappings",
                    genai_schema.types.len(),
                    genai_schema.enums.len(),
                    lookup.len()
                );
                Some(lookup)
            }
            Err(e) => {
                eprintln!("Warning: failed to read js-genai source: {}", e);
                None
            }
        }
    } else {
        None
    };

    // Step 2: Read ADK-JS TypeScript source
    let schema = match reader::read_source_dir(source) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading source: {}", e);
            std::process::exit(1);
        }
    };

    eprintln!(
        "Extracted {} agents, {} tools, {} types",
        schema.agents.len(),
        schema.tools.len(),
        schema.types.len()
    );

    // Step 3: Generate compilable Rust code with genai type lookup
    let rust_code = if let Some(lookup) = genai_lookup {
        codegen::generate_compilable_with_genai(&schema, &lookup)
    } else {
        codegen::generate_compilable(&schema)
    };

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    std::fs::write(output, &rust_code)
        .unwrap_or_else(|e| panic!("Failed to write {}: {}", output.display(), e));

    eprintln!("Compilable Rust code written to: {}", output.display());

    // Print summary
    println!("=== Transpile Summary ===");
    println!("Source: {}", source.display());
    if let Some(gs) = genai_source {
        println!("Genai source: {}", gs.display());
    }
    println!("Output: {}", output.display());
    println!("Framework: {}", schema.source.framework);
    println!("Agents: {}", schema.agents.len());
    println!("Tools: {}", schema.tools.len());
    println!("Types: {}", schema.types.len());
    println!("Output size: {} bytes", rust_code.len());
    println!("Target: rs-adk (compilable)");
}
