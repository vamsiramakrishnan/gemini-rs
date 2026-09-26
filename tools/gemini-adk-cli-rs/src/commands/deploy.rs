//! `adk deploy`: put adk-runtime in front of a bundle store.
//!
//! The deployed service is always the same image (`deploy/Dockerfile`:
//! adk-runtime and adk); what it serves comes from the bundle store
//! (`--bundles`, `--serve`). Every command is printed before it runs, and
//! `--dry-run` prints without running. See docs/user-guide/deploy.md.

use std::path::Path;
use std::process::Command;

/// Deployment target.
#[derive(Debug, Clone)]
pub enum Target {
    CloudRun,
    Gke,
    AgentEngine,
}

/// Configuration for the deploy command.
pub struct DeployConfig {
    pub target: Target,
    pub project: Option<String>,
    pub region: String,
    pub service_name: String,
    /// Bundle store URI (`gs://bucket/prefix`).
    pub bundles: Option<String>,
    /// `ADK_SERVE`: comma-separated references such as `booking:prod`.
    pub serve: Option<String>,
    /// Deploy this image instead of building one.
    pub image: Option<String>,
    /// The gemini-rs checkout holding `deploy/Dockerfile`.
    pub source: String,
    /// Secret Manager secret holding `ADK_RUNTIME_TOKENS`.
    pub tokens_secret: String,
    /// Secret Manager secret holding `TWILIO_AUTH_TOKEN`, for phone calls.
    pub twilio_secret: Option<String>,
    pub service_account: Option<String>,
    pub max_sessions: u32,
    pub min_instances: u32,
    pub dry_run: bool,
}

/// adk-runtime's port. The image sets `PORT=8080`, and Cloud Run is told
/// the same, so the two cannot disagree.
const PORT: u16 = 8080;

/// Cloud Run sends SIGTERM and kills the container 10 seconds later.
const CLOUD_RUN_DRAIN_SECS: u32 = 8;

pub fn run(config: DeployConfig) -> Result<(), Box<dyn std::error::Error>> {
    let plan = match config.target {
        Target::CloudRun => cloud_run_plan(&config)?,
        Target::Gke => gke_plan(&config)?,
        Target::AgentEngine => {
            println!(
                "Agent Engine is not a supported deployment target. adk-runtime runs as a \
                 container; deploy it to Cloud Run instead:\n\n  \
                 adk deploy cloud-run --project {} --bundles gs://BUCKET/bundles --serve NAME:prod\n",
                config.project.as_deref().unwrap_or("<PROJECT>"),
            );
            return Err("Agent Engine deployment is not supported; use cloud-run".into());
        }
    };
    for step in &plan.steps {
        println!("+ {}", shell_line(step));
        if !config.dry_run {
            let status = Command::new(&step[0])
                .args(&step[1..])
                .status()
                .map_err(|e| {
                    format!(
                        "could not run {}: {e} (is the Google Cloud CLI installed?)",
                        step[0]
                    )
                })?;
            if !status.success() {
                return Err(format!("`{}` failed ({status})", step[0]).into());
            }
        }
    }
    if !plan.after.is_empty() {
        println!("\n{}", plan.after);
    }
    Ok(())
}

/// The commands a deploy runs, then what to do next.
#[derive(Debug)]
struct Plan {
    steps: Vec<Vec<String>>,
    after: String,
}

fn required<'a>(value: &'a Option<String>, flag: &str) -> Result<&'a str, String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("{flag} is required"))
}

fn image(config: &DeployConfig, project: &str) -> String {
    config.image.clone().unwrap_or_else(|| {
        format!(
            "{}-docker.pkg.dev/{project}/adk/{}:latest",
            config.region, config.service_name
        )
    })
}

/// Build the image with Cloud Build, unless `--image` names one.
fn build_step(
    config: &DeployConfig,
    project: &str,
    image: &str,
) -> Result<Option<Vec<String>>, String> {
    if config.image.is_some() {
        return Ok(None);
    }
    let source = Path::new(&config.source);
    if !source.join("deploy/Dockerfile").is_file() {
        return Err(format!(
            "{} has no deploy/Dockerfile: run from a gemini-rs checkout, pass --source <checkout>, \
             or pass --image to deploy an image you built",
            source.display()
        ));
    }
    Ok(Some(argv([
        "gcloud",
        "builds",
        "submit",
        &config.source,
        "--project",
        project,
        "--config",
        &source.join("deploy/cloudbuild.yaml").display().to_string(),
        "--substitutions",
        &format!("_IMAGE={image}"),
    ])))
}

fn cloud_run_plan(config: &DeployConfig) -> Result<Plan, String> {
    let project = required(&config.project, "--project")?;
    let bundles = required(&config.bundles, "--bundles (or ADK_BUNDLES)")?;
    let serve = required(&config.serve, "--serve")?;
    if !bundles.starts_with("gs://") {
        return Err(format!(
            "--bundles {bundles}: a deployed runtime needs a Cloud Storage store (gs://bucket/prefix)"
        ));
    }
    let image = image(config, project);
    let service_account = config
        .service_account
        .clone()
        .unwrap_or_else(|| format!("adk-runtime@{project}.iam.gserviceaccount.com"));

    // ADK_SERVE holds commas, so the list uses `;` as its delimiter
    // (see `gcloud topic escaping`).
    let env = [
        format!("ADK_BUNDLES={bundles}"),
        format!("ADK_SERVE={serve}"),
        format!("ADK_MAX_SESSIONS={}", config.max_sessions),
        format!("ADK_DRAIN_SECS={CLOUD_RUN_DRAIN_SECS}"),
        "GOOGLE_GENAI_USE_VERTEXAI=true".into(),
        format!("GOOGLE_CLOUD_PROJECT={project}"),
        format!("GOOGLE_CLOUD_LOCATION={}", config.region),
    ]
    .join(";");
    let mut secrets = vec![format!(
        "ADK_RUNTIME_TOKENS={}:latest",
        config.tokens_secret
    )];
    if let Some(twilio) = &config.twilio_secret {
        secrets.push(format!("TWILIO_AUTH_TOKEN={twilio}:latest"));
    }

    let mut steps: Vec<Vec<String>> = build_step(config, project, &image)?.into_iter().collect();
    steps.push(argv([
        "gcloud",
        "run",
        "deploy",
        &config.service_name,
        "--project",
        project,
        "--region",
        &config.region,
        "--image",
        &image,
        "--port",
        &PORT.to_string(),
        "--service-account",
        &service_account,
        // A session is one request: allow the longest Cloud Run permits.
        "--timeout",
        "3600",
        "--concurrency",
        &config.max_sessions.to_string(),
        "--min-instances",
        &config.min_instances.to_string(),
        "--session-affinity",
        // WebSocket sessions work between requests.
        "--no-cpu-throttling",
        "--cpu-boost",
        // Browsers and Twilio cannot present Google identity tokens;
        // adk-runtime authenticates sessions itself.
        "--allow-unauthenticated",
        "--set-env-vars",
        &format!("^;^{env}"),
        "--set-secrets",
        &secrets.join(","),
    ]));
    Ok(Plan {
        steps,
        after: format!(
            "Sessions: wss://<service URL>/ws/<bundle> with a token from the {} secret.\n\
             Promote or roll back without redeploying: adk bundle label <bundle> prod <version> \
             --store {bundles}",
            config.tokens_secret
        ),
    })
}

fn gke_plan(config: &DeployConfig) -> Result<Plan, String> {
    let project = required(&config.project, "--project")?;
    let image = image(config, project);
    let steps: Vec<Vec<String>> = build_step(config, project, &image)?.into_iter().collect();
    Ok(Plan {
        steps,
        after: format!(
            "Next, with kubectl pointed at your cluster:\n  \
             1. Replace PROJECT_ID, REGION, BUCKET and the host in {src}/deploy/gke/*.yaml,\n     \
             and set ADK_SERVE in deployment.yaml.\n  \
             2. kubectl create secret generic adk-runtime-tokens --from-literal=tokens=<token>\n  \
             3. kubectl apply -f {src}/deploy/gke/\n  \
             4. kubectl set image deployment/adk-runtime runtime={image}\n\
             See docs/user-guide/deploy.md for Workload Identity.",
            src = config.source,
        ),
    })
}

fn argv<const N: usize>(parts: [&str; N]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// `argv` as one line a POSIX shell would run unchanged.
fn shell_line(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            let plain = !arg.is_empty()
                && arg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./:=@,+".contains(c));
            if plain {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> DeployConfig {
        DeployConfig {
            target: Target::CloudRun,
            project: Some("my-project".into()),
            region: "us-central1".into(),
            service_name: "adk-runtime".into(),
            bundles: Some("gs://my-bucket/bundles".into()),
            serve: Some("booking:prod,clinic:prod".into()),
            image: None,
            // The repository root, which has deploy/Dockerfile.
            source: concat!(env!("CARGO_MANIFEST_DIR"), "/../..").into(),
            tokens_secret: "adk-runtime-tokens".into(),
            twilio_secret: None,
            service_account: None,
            max_sessions: 50,
            min_instances: 1,
            dry_run: true,
        }
    }

    #[test]
    fn cloud_run_builds_the_runtime_image_then_deploys_it_on_port_8080() {
        let plan = cloud_run_plan(&config()).unwrap();
        assert_eq!(plan.steps.len(), 2);
        let build = shell_line(&plan.steps[0]);
        assert!(build.starts_with("gcloud builds submit"), "{build}");
        assert!(build.contains("deploy/cloudbuild.yaml"), "{build}");
        assert!(
            build.contains("_IMAGE=us-central1-docker.pkg.dev/my-project/adk/adk-runtime:latest"),
            "{build}"
        );

        let deploy = &plan.steps[1];
        let flag = |name: &str| {
            let at = deploy
                .iter()
                .position(|a| a == name)
                .unwrap_or_else(|| panic!("no {name}"));
            deploy[at + 1].clone()
        };
        assert_eq!(flag("--port"), "8080");
        assert_eq!(flag("--timeout"), "3600");
        assert_eq!(flag("--concurrency"), "50");
        assert_eq!(
            flag("--service-account"),
            "adk-runtime@my-project.iam.gserviceaccount.com"
        );
        assert_eq!(
            flag("--set-secrets"),
            "ADK_RUNTIME_TOKENS=adk-runtime-tokens:latest"
        );
        assert!(deploy.contains(&"--session-affinity".to_string()));
        assert!(deploy.contains(&"--no-cpu-throttling".to_string()));
        // The comma in ADK_SERVE survives because `;` is the delimiter.
        let env = flag("--set-env-vars");
        assert!(env.starts_with("^;^"), "{env}");
        assert!(
            env.contains(";ADK_SERVE=booking:prod,clinic:prod;"),
            "{env}"
        );
        assert!(env.contains("ADK_DRAIN_SECS=8"), "{env}");
    }

    #[test]
    fn a_given_image_skips_the_build_and_twilio_adds_its_secret() {
        let mut config = config();
        config.image = Some("example.com/adk-runtime:v1".into());
        config.twilio_secret = Some("twilio-auth-token".into());
        let plan = cloud_run_plan(&config).unwrap();
        assert_eq!(plan.steps.len(), 1);
        let line = shell_line(&plan.steps[0]);
        assert!(
            line.contains("--image example.com/adk-runtime:v1"),
            "{line}"
        );
        assert!(
            line.contains("ADK_RUNTIME_TOKENS=adk-runtime-tokens:latest,TWILIO_AUTH_TOKEN=twilio-auth-token:latest"),
            "{line}"
        );
    }

    #[test]
    fn missing_or_local_stores_are_refused() {
        let mut config = config();
        config.serve = None;
        assert!(cloud_run_plan(&config).unwrap_err().contains("--serve"));
        let mut config = super::tests::config();
        config.bundles = Some("./bundles".into());
        assert!(cloud_run_plan(&config).unwrap_err().contains("gs://"));
        let mut config = super::tests::config();
        config.source = "/nonexistent".into();
        assert!(
            cloud_run_plan(&config)
                .unwrap_err()
                .contains("deploy/Dockerfile")
        );
    }

    #[test]
    fn printed_commands_can_be_pasted_into_a_shell() {
        let line = shell_line(&argv(["gcloud", "--set-env-vars", "^;^A=1;B=x y", "it's"]));
        assert_eq!(line, r#"gcloud --set-env-vars '^;^A=1;B=x y' 'it'\''s'"#);
    }
}
