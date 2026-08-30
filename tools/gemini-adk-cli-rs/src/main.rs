mod commands;
mod manifest;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "adk", version, about = "Agent Development Kit CLI for Gemini")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold a new agent project.
    Create {
        /// Name of the agent project to create.
        name: String,
        /// Model to use (default: gemini-2.0-flash).
        #[arg(long, default_value = "gemini-2.0-flash")]
        model: String,
        /// Google AI API key to write into .env.
        #[arg(long)]
        api_key: Option<String>,
    },

    /// Interactive terminal REPL for an agent.
    Run {
        /// Path to the agent directory containing agent.toml.
        agent_dir: String,
        /// Save session transcript to a JSON file on exit.
        #[arg(long)]
        save_session: Option<String>,
        /// Session ID to resume (if supported by session service).
        #[arg(long)]
        session_id: Option<String>,
        /// Replay a previously saved session file instead of interactive input.
        #[arg(long)]
        replay: Option<String>,
    },

    /// Replay and validate a captured Live session event log.
    Replay {
        /// Path to a JSON event log or object containing an `events` array.
        path: String,
        /// Optional runtime contract JSON exported from DevTools or SDK.
        #[arg(long)]
        contract: Option<String>,
    },

    /// Start a development web server with UI.
    Web {
        /// Path to the agent directory containing agent.toml.
        agent_dir: String,
        /// Host to bind to.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port to listen on.
        #[arg(long, default_value_t = 8000)]
        port: u16,
        /// Comma-separated list of allowed CORS origins.
        #[arg(long)]
        allow_origins: Option<String>,
        /// Log level (trace, debug, info, warn, error).
        #[arg(long, default_value = "info")]
        log_level: String,
        /// Enable auto-reload on file changes.
        #[arg(long)]
        reload: bool,
        /// Enable Agent-to-Agent (A2A) protocol endpoint.
        #[arg(long)]
        a2a: bool,
        /// Export traces to Google Cloud Trace.
        #[arg(long)]
        trace_to_cloud: bool,
        /// URI for external session service.
        #[arg(long)]
        session_service_uri: Option<String>,
        /// URI for external artifact storage.
        #[arg(long)]
        artifact_storage_uri: Option<String>,
    },

    /// Start a headless API server (no UI).
    Api {
        /// Path to the agent directory containing agent.toml.
        agent_dir: String,
        /// Host to bind to.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port to listen on.
        #[arg(long, default_value_t = 8000)]
        port: u16,
        /// Comma-separated list of allowed CORS origins.
        #[arg(long)]
        allow_origins: Option<String>,
        /// Log level (trace, debug, info, warn, error).
        #[arg(long, default_value = "info")]
        log_level: String,
        /// Enable auto-reload on file changes.
        #[arg(long)]
        reload: bool,
        /// Enable Agent-to-Agent (A2A) protocol endpoint.
        #[arg(long)]
        a2a: bool,
        /// Export traces to Google Cloud Trace.
        #[arg(long)]
        trace_to_cloud: bool,
        /// URI for external session service.
        #[arg(long)]
        session_service_uri: Option<String>,
        /// URI for external artifact storage.
        #[arg(long)]
        artifact_storage_uri: Option<String>,
    },

    /// Run evaluations against an agent.
    Eval {
        /// Path to the agent directory containing agent.toml.
        agent_dir: String,
        /// Path to the .evalset.json evaluation set file.
        evalset_path: String,
        /// Path to a test_config.json file with scoring criteria.
        #[arg(long)]
        config_file: Option<String>,
        /// Print detailed per-case results.
        #[arg(long)]
        print_detailed_results: bool,
    },

    /// Check environment setup (API keys, toolchain, credentials).
    Doctor,

    /// Conversation-compiler devtools: inspect, graph, and simulate a spec.
    Flow {
        #[command(subcommand)]
        action: FlowAction,
    },

    /// Session record/replay utilities (wire logs from `record_wire(..)`).
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },

    /// Deploy an agent to a cloud target.
    Deploy {
        /// Deployment target: cloud_run, gke, or agent_engine.
        target: DeployTarget,
        /// Path to the agent directory containing agent.toml.
        agent_dir: String,
        /// Google Cloud project ID.
        #[arg(long)]
        project: Option<String>,
        /// Google Cloud region.
        #[arg(long, default_value = "us-central1")]
        region: String,
        /// Cloud Run / GKE service name override.
        #[arg(long)]
        service_name: Option<String>,
        /// Bundle the web UI with the deployment.
        #[arg(long)]
        with_ui: bool,
        /// Export traces to Google Cloud Trace.
        #[arg(long)]
        trace_to_cloud: bool,
    },
}

#[derive(Clone, Debug, clap::ValueEnum)]
enum DeployTarget {
    CloudRun,
    Gke,
    AgentEngine,
}

#[derive(Subcommand)]
enum SessionAction {
    /// Replay a recorded wire log offline through the real L1 processor.
    ///
    /// Re-processes the recorded frames only — no LLM re-execution, no tool
    /// re-execution. Prints a turn-by-turn summary (events, tool calls, final
    /// state keys); with --journal, diffs the replayed final state against the
    /// recorded mutation journal and reports CLEAN or DRIFT.
    Replay {
        /// Path to a wire log (JSONL) recorded via `record_wire(..)`.
        wire_log: String,
        /// Optional state-mutation journal (JSONL) written by FileJournalSink.
        #[arg(long)]
        journal: Option<String>,
    },
}

#[derive(Subcommand)]
enum FlowAction {
    /// Print a summary of a compiled conversation spec (JSON file).
    Inspect {
        /// Path to a ConversationSpec JSON file.
        spec: String,
    },
    /// Render the governed flow as a Mermaid diagram.
    Graph {
        /// Path to a ConversationSpec JSON file.
        spec: String,
    },
    /// Print the JSON Schema for a ConversationSpec (the authoring contract).
    Schema,
    /// Compile a spec and report errors as JSON (exits non-zero on failure).
    Validate {
        /// Path to a ConversationSpec JSON file.
        spec: String,
    },
    /// Run a model-free Scenario (JSON) against a conversation spec.
    Simulate {
        /// Path to a ConversationSpec JSON file.
        spec: String,
        /// Path to a Scenario JSON file.
        scenario: String,
    },
    /// Conversation CI: compile every spec in a directory and run its scenarios.
    Ci {
        /// Directory holding `*.spec.json` + `*.scenario.json` files (recursive).
        dir: String,
        /// Emit a machine-readable JSON report instead of the human summary.
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Create {
            name,
            model,
            api_key,
        } => commands::create::run(&name, &model, api_key.as_deref())?,

        Command::Run {
            agent_dir,
            save_session,
            session_id,
            replay,
        } => {
            commands::run::run(
                &agent_dir,
                save_session.as_deref(),
                session_id.as_deref(),
                replay.as_deref(),
            )
            .await?
        }

        Command::Replay { path, contract } => commands::replay::run(&path, contract.as_deref())?,

        Command::Web {
            agent_dir,
            host,
            port,
            allow_origins,
            log_level,
            reload,
            a2a,
            trace_to_cloud,
            session_service_uri,
            artifact_storage_uri,
        } => {
            commands::web::run(commands::web::WebConfig {
                agent_dir,
                host,
                port,
                allow_origins,
                log_level,
                reload,
                a2a,
                trace_to_cloud,
                session_service_uri,
                artifact_storage_uri,
            })
            .await?
        }

        Command::Api {
            agent_dir,
            host,
            port,
            allow_origins,
            log_level,
            reload,
            a2a,
            trace_to_cloud,
            session_service_uri,
            artifact_storage_uri,
        } => {
            commands::api::run(commands::api::ApiConfig {
                agent_dir,
                host,
                port,
                allow_origins,
                log_level,
                reload,
                a2a,
                trace_to_cloud,
                session_service_uri,
                artifact_storage_uri,
            })
            .await?
        }

        Command::Eval {
            agent_dir,
            evalset_path,
            config_file,
            print_detailed_results,
        } => {
            commands::eval::run(
                &agent_dir,
                &evalset_path,
                config_file.as_deref(),
                print_detailed_results,
            )
            .await?
        }

        Command::Doctor => commands::doctor::run()?,

        Command::Session { action } => match action {
            SessionAction::Replay { wire_log, journal } => {
                commands::session::replay(&wire_log, journal.as_deref()).await?
            }
        },

        Command::Flow { action } => match action {
            FlowAction::Inspect { spec } => commands::flow::inspect(&spec)?,
            FlowAction::Graph { spec } => commands::flow::graph(&spec)?,
            FlowAction::Schema => commands::flow::schema()?,
            FlowAction::Validate { spec } => commands::flow::validate(&spec)?,
            FlowAction::Simulate { spec, scenario } => {
                commands::flow::simulate(&spec, &scenario).await?
            }
            FlowAction::Ci { dir, json } => commands::flow::ci(&dir, json).await?,
        },

        Command::Deploy {
            target,
            agent_dir,
            project,
            region,
            service_name,
            with_ui,
            trace_to_cloud,
        } => commands::deploy::run(commands::deploy::DeployConfig {
            target: match target {
                DeployTarget::CloudRun => commands::deploy::Target::CloudRun,
                DeployTarget::Gke => commands::deploy::Target::Gke,
                DeployTarget::AgentEngine => commands::deploy::Target::AgentEngine,
            },
            agent_dir,
            project,
            region,
            service_name,
            with_ui,
            trace_to_cloud,
        })?,
    }

    Ok(())
}
