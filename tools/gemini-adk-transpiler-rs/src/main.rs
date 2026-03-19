mod codegen;
mod diff;
mod readers;
mod schema;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "gemini-adk-transpiler-rs")]
#[command(about = "Transpiles ADK-JS, js-genai, and adk-fluent sources to Rust")]
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
    /// One-shot: read ADK-JS source and generate compilable Rust code targeting gemini-adk-rs
    Transpile {
        /// Path to the ADK-JS source directory (e.g. /tmp/adk-js/core/src/)
        #[arg(short, long)]
        source: PathBuf,

        /// Output Rust source file path (e.g. crates/gemini-adk-rs/src/agents/generated.rs)
        #[arg(short, long)]
        output: PathBuf,

        /// Optional: path to js-genai source for precise type resolution
        #[arg(short, long)]
        genai_source: Option<PathBuf>,
    },
    /// Transpile js-genai types into per-module Rust files for gemini-live
    TranspileGenai {
        /// Path to the js-genai source directory (e.g. /tmp/js-genai/src/)
        #[arg(short, long)]
        source: PathBuf,

        /// Output directory for generated Rust modules (e.g. crates/gemini-live/src/generated/)
        #[arg(short, long)]
        output_dir: PathBuf,
    },
    /// Transpile js-genai REST module classes into per-module Rust client code
    TranspileRest {
        /// Path to the js-genai source directory (e.g. /tmp/js-genai/src/)
        #[arg(short, long)]
        source: PathBuf,

        /// Output directory for generated Rust modules (e.g. crates/gemini-live/src/)
        #[arg(short, long)]
        output_dir: PathBuf,

        /// Modules to generate (comma-separated, e.g. "files,caches,batches")
        /// If not specified, generates all detected modules
        #[arg(short, long)]
        modules: Option<String>,
    },
    /// Read adk-fluent Python source and extract builder/factory/operator definitions
    ReadFluent {
        /// Path to the adk-fluent source directory (e.g. /tmp/adk-fluent/src/adk_fluent/)
        #[arg(short, long)]
        source: PathBuf,

        /// Output JSON file path
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
        Commands::TranspileGenai { source, output_dir } => {
            run_transpile_genai(&source, &output_dir);
        }
        Commands::TranspileRest {
            source,
            output_dir,
            modules,
        } => {
            run_transpile_rest(&source, &output_dir, modules.as_deref());
        }
        Commands::ReadFluent { source, output } => {
            run_read_fluent(&source, &output);
        }
    }
}

fn run_read(source: &Path, output: &Path) {
    eprintln!("Reading TypeScript source from: {}", source.display());

    match readers::read_source_dir(source) {
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

    match readers::read_genai_source(source) {
        Ok(schema) => {
            let with_wire = schema
                .types
                .iter()
                .filter(|t| t.has_wire_equivalent)
                .count();
            let total_types = schema.types.len();
            let with_wire_enums = schema
                .enums
                .iter()
                .filter(|e| e.has_wire_equivalent)
                .count();
            let total_enums = schema.enums.len();

            eprintln!(
                "Extracted {} types ({} with wire equiv), {} enums ({} with wire equiv), {} aliases, {} helpers",
                total_types, with_wire,
                total_enums, with_wire_enums,
                schema.type_aliases.len(),
                schema.helpers.len()
            );

            let json = serde_json::to_string_pretty(&schema).expect("Failed to serialize schema");

            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent).ok();
            }

            std::fs::write(output, &json)
                .unwrap_or_else(|e| panic!("Failed to write {}: {}", output.display(), e));

            eprintln!("Schema written to: {}", output.display());

            println!("=== js-genai Schema Summary ===");
            println!("Source: {}", schema.source.source_dir);
            println!();
            println!(
                "Types: {} total, {} with wire equivalent",
                total_types, with_wire
            );
            println!();

            println!("Wire Mappings:");
            for t in &schema.types {
                if let Some(ref wire) = t.wire_type {
                    println!("  {} -> {}", t.name, wire);
                }
            }
            for e in &schema.enums {
                if let Some(ref wire) = e.wire_type {
                    println!("  {} -> {}", e.name, wire);
                }
            }

            println!();
            println!(
                "Types without wire equivalent ({}):",
                total_types - with_wire
            );
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
                    .map(|w| format!(" -> {}", w))
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

    println!("=== Codegen Summary ===");
    println!("Source framework: {}", schema.source.framework);
    println!("Agents generated: {}", schema.agents.len());
    println!("Tools generated: {}", schema.tools.len());
    println!("Types generated: {}", schema.types.len());
    println!("Output size: {} bytes", rust_code.len());
}

fn run_transpile(source: &Path, output: &Path, genai_source: Option<&Path>) {
    eprintln!("Transpiling ADK-JS source from: {}", source.display());

    let genai_lookup = if let Some(genai_path) = genai_source {
        eprintln!("Reading js-genai types from: {}", genai_path.display());
        match readers::read_genai_source(genai_path) {
            Ok(genai_schema) => {
                let lookup = readers::build_type_lookup(&genai_schema);
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

    let schema = match readers::read_source_dir(source) {
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
    println!("Target: gemini-adk-rs (compilable)");
}

fn run_transpile_genai(source: &Path, output_dir: &Path) {
    eprintln!("Transpiling js-genai types from: {}", source.display());

    let schema = match readers::read_genai_source(source) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading source: {}", e);
            std::process::exit(1);
        }
    };

    let with_wire = schema
        .types
        .iter()
        .filter(|t| t.has_wire_equivalent)
        .count();
    let without_wire = schema.types.len() - with_wire;

    eprintln!(
        "Extracted {} types ({} with wire equiv, {} to generate)",
        schema.types.len(),
        with_wire,
        without_wire
    );

    let output = codegen::generate_genai_modules(&schema);

    if let Err(e) = codegen::genai::write_genai_modules(&output, output_dir) {
        eprintln!("Error writing output: {}", e);
        std::process::exit(1);
    }

    println!("=== Transpile Genai Summary ===");
    println!("Source: {}", source.display());
    println!("Output dir: {}", output_dir.display());
    println!("Modules generated: {}", output.modules.len());
    for (name, code) in &output.modules {
        println!("  - {}.rs ({} bytes)", name, code.len());
    }
    println!("Types with existing wire equiv (skipped): {}", with_wire);
    println!("Types generated: {}", without_wire);
}

fn run_transpile_rest(source: &Path, output_dir: &Path, modules_filter: Option<&str>) {
    eprintln!("Reading REST modules from: {}", source.display());

    let mut rest_modules = match readers::read_rest_modules(source) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Error reading REST modules: {}", e);
            std::process::exit(1);
        }
    };

    if let Some(filter) = modules_filter {
        let allowed: Vec<&str> = filter.split(',').map(|s| s.trim()).collect();
        rest_modules.retain(|m| allowed.contains(&m.name.as_str()));
    }

    eprintln!("Found {} REST modules", rest_modules.len());
    for m in &rest_modules {
        eprintln!("  - {} ({} methods)", m.name, m.methods.len());
    }

    let output = codegen::generate_rest_modules(&rest_modules);

    if let Err(e) = codegen::rest::write_rest_modules(&output, output_dir) {
        eprintln!("Error writing output: {}", e);
        std::process::exit(1);
    }

    println!("=== Transpile REST Summary ===");
    println!("Source: {}", source.display());
    println!("Output dir: {}", output_dir.display());
    println!("Modules generated: {}", output.modules.len());
    for (name, code) in &output.modules {
        println!("  - {}/mod.rs ({} bytes)", name, code.len());
    }
}

fn run_read_fluent(source: &Path, output: &Path) {
    eprintln!(
        "Reading adk-fluent Python source from: {}",
        source.display()
    );

    match readers::read_fluent_source(source) {
        Ok(schema) => {
            eprintln!(
                "Extracted {} builder methods, {} factories, {} operators, {} workflows",
                schema.builder_methods.len(),
                schema.factories.len(),
                schema.operators.len(),
                schema.workflows.len(),
            );

            let json = serde_json::to_string_pretty(&schema).expect("Failed to serialize schema");

            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent).ok();
            }

            std::fs::write(output, &json)
                .unwrap_or_else(|e| panic!("Failed to write {}: {}", output.display(), e));

            eprintln!("Schema written to: {}", output.display());

            println!("=== adk-fluent Schema Summary ===");
            println!("Source: {}", schema.source.source_dir);
            println!();

            println!("Builder Methods ({}):", schema.builder_methods.len());
            for m in &schema.builder_methods {
                println!("  - {}({}) -> {}", m.name, m.param_type, m.rust_type);
            }

            println!();
            println!("Factory Functions ({}):", schema.factories.len());

            // Group by module
            let mut by_module = std::collections::HashMap::new();
            for f in &schema.factories {
                by_module
                    .entry(f.module.rust_module())
                    .or_insert_with(Vec::new)
                    .push(f);
            }
            for (module, fns) in &by_module {
                println!("  {} ({} functions):", module.to_uppercase(), fns.len());
                for f in fns {
                    let params: Vec<String> = f
                        .params
                        .iter()
                        .map(|p| {
                            if let Some(ref d) = p.default {
                                format!("{}: {} = {}", p.name, p.py_type, d)
                            } else {
                                format!("{}: {}", p.name, p.py_type)
                            }
                        })
                        .collect();
                    println!(
                        "    - {}({}) -> {}",
                        f.name,
                        params.join(", "),
                        f.return_type
                    );
                }
            }

            println!();
            println!("Operators ({}):", schema.operators.len());
            for op in &schema.operators {
                println!("  - {} {} {} -> {}", op.lhs, op.operator, op.rhs, op.output);
            }

            println!();
            println!("Workflows ({}):", schema.workflows.len());
            for w in &schema.workflows {
                println!("  - {} [{} methods]", w.name, w.methods.len());
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
