use crate::manifest;
use std::path::Path;

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
    pub agent_dir: String,
    pub project: Option<String>,
    pub region: String,
    pub service_name: Option<String>,
    pub with_ui: bool,
    pub trace_to_cloud: bool,
}

const DOCKERFILE_TEMPLATE: &str = r#"FROM rust:1.82-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/{{SERVICE_NAME}} /usr/local/bin/agent
ENV PORT=8080
EXPOSE 8080
CMD ["agent"]
"#;

const K8S_TEMPLATE: &str = r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: {{SERVICE_NAME}}
  labels:
    app: {{SERVICE_NAME}}
spec:
  replicas: 1
  selector:
    matchLabels:
      app: {{SERVICE_NAME}}
  template:
    metadata:
      labels:
        app: {{SERVICE_NAME}}
    spec:
      containers:
      - name: agent
        image: {{IMAGE}}
        ports:
        - containerPort: 8080
        env:
        - name: PORT
          value: "8080"
        resources:
          requests:
            memory: "256Mi"
            cpu: "250m"
          limits:
            memory: "512Mi"
            cpu: "500m"
---
apiVersion: v1
kind: Service
metadata:
  name: {{SERVICE_NAME}}
spec:
  selector:
    app: {{SERVICE_NAME}}
  ports:
  - port: 80
    targetPort: 8080
  type: LoadBalancer
"#;

pub fn run(config: DeployConfig) -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(&config.agent_dir);
    let agent = manifest::load_manifest(&dir.join("agent.toml"))?;
    let service_name = config
        .service_name
        .as_deref()
        .unwrap_or(&agent.name);

    match config.target {
        Target::CloudRun => deploy_cloud_run(&config, &agent, service_name),
        Target::Gke => deploy_gke(&config, &agent, service_name),
        Target::AgentEngine => deploy_agent_engine(&config, &agent),
    }
}

fn deploy_cloud_run(
    config: &DeployConfig,
    agent: &manifest::AgentManifest,
    service_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let project = config
        .project
        .as_deref()
        .ok_or("--project is required for Cloud Run deployment")?;

    println!("Deploying agent '{}' to Cloud Run", agent.name);
    println!("  Project:  {}", project);
    println!("  Region:   {}", config.region);
    println!("  Service:  {}", service_name);

    // Generate Dockerfile
    let dockerfile = DOCKERFILE_TEMPLATE.replace("{{SERVICE_NAME}}", service_name);
    let dockerfile_path = Path::new(&config.agent_dir).join("Dockerfile");
    std::fs::write(&dockerfile_path, &dockerfile)?;
    println!("  Generated: {}", dockerfile_path.display());

    // Print the gcloud command that would be run
    let mut cmd = format!(
        "gcloud run deploy {service_name} \
         --source . \
         --project {project} \
         --region {} \
         --allow-unauthenticated",
        config.region,
    );
    if config.with_ui {
        cmd.push_str(" --set-env-vars SERVE_UI=true");
    }
    if config.trace_to_cloud {
        cmd.push_str(" --set-env-vars TRACE_TO_CLOUD=true");
    }

    println!("\nRun the following command to deploy:");
    println!("  cd {} && {}", config.agent_dir, cmd);

    Ok(())
}

fn deploy_gke(
    config: &DeployConfig,
    agent: &manifest::AgentManifest,
    service_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let project = config
        .project
        .as_deref()
        .ok_or("--project is required for GKE deployment")?;

    let image = format!(
        "gcr.io/{}/{}:latest",
        project, service_name
    );

    println!("Deploying agent '{}' to GKE", agent.name);
    println!("  Project: {}", project);
    println!("  Region:  {}", config.region);
    println!("  Image:   {}", image);

    // Generate Dockerfile
    let dockerfile = DOCKERFILE_TEMPLATE.replace("{{SERVICE_NAME}}", service_name);
    let dockerfile_path = Path::new(&config.agent_dir).join("Dockerfile");
    std::fs::write(&dockerfile_path, &dockerfile)?;
    println!("  Generated: {}", dockerfile_path.display());

    // Generate K8s manifests
    let k8s = K8S_TEMPLATE
        .replace("{{SERVICE_NAME}}", service_name)
        .replace("{{IMAGE}}", &image);
    let k8s_path = Path::new(&config.agent_dir).join("k8s.yaml");
    std::fs::write(&k8s_path, &k8s)?;
    println!("  Generated: {}", k8s_path.display());

    println!("\nRun the following commands to deploy:");
    println!("  cd {}", config.agent_dir);
    println!("  docker build -t {} .", image);
    println!("  docker push {}", image);
    println!("  kubectl apply -f k8s.yaml");

    Ok(())
}

fn deploy_agent_engine(
    config: &DeployConfig,
    agent: &manifest::AgentManifest,
) -> Result<(), Box<dyn std::error::Error>> {
    let project = config
        .project
        .as_deref()
        .ok_or("--project is required for Agent Engine deployment")?;

    println!("Deploying agent '{}' to Vertex AI Agent Engine", agent.name);
    println!("  Project: {}", project);
    println!("  Region:  {}", config.region);

    // TODO: Call Vertex AI Agent Engine API to register/deploy the agent.
    // This requires:
    //   1. Package agent code as a Cloud Function or container
    //   2. POST to https://{region}-aiplatform.googleapis.com/v1/projects/{project}/locations/{region}/agents
    //   3. Wait for operation to complete

    println!("\nAgent Engine deployment is not yet implemented.");
    println!("See: https://cloud.google.com/vertex-ai/docs/agents/deploy");

    Ok(())
}
