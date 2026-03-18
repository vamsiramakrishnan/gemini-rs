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
