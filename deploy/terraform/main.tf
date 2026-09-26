# adk-runtime on Cloud Run: the service, its service account and
# permissions, the bundle bucket, and the client-token secret.
#
# Prerequisites: the Cloud Run, Vertex AI, Secret Manager and Cloud Storage
# APIs are enabled, and var.image has been built and pushed (see
# deploy/cloudbuild.yaml). Then:
#
#   terraform init
#   # 1. The bucket first: the runtime refuses to start until every
#   #    reference in var.serve resolves.
#   terraform apply -target google_storage_bucket.bundles -var ...
#   adk bundle push agent.json --store gs://BUCKET/bundles --label prod
#   # 2. Everything else.
#   terraform apply -var project=... -var image=... -var bucket=... \
#     -var serve=booking:prod -var runtime_tokens=...

terraform {
  required_version = ">= 1.5"
  required_providers {
    google = {
      source  = "hashicorp/google"
      version = ">= 6.0"
    }
  }
}

provider "google" {
  project = var.project
  region  = var.region
}

resource "google_service_account" "runtime" {
  account_id   = "adk-runtime"
  display_name = "adk-runtime"
}

# Live sessions and extraction on Vertex AI.
resource "google_project_iam_member" "vertex" {
  project = var.project
  role    = "roles/aiplatform.user"
  member  = "serviceAccount:${google_service_account.runtime.email}"
}

resource "google_storage_bucket" "bundles" {
  name                        = var.bucket
  location                    = var.region
  uniform_bucket_level_access = true
}

# The runtime only reads bundles. Whoever pushes and labels them (a person,
# or CI) needs roles/storage.objectAdmin on this bucket.
resource "google_storage_bucket_iam_member" "runtime_reads_bundles" {
  bucket = google_storage_bucket.bundles.name
  role   = "roles/storage.objectViewer"
  member = "serviceAccount:${google_service_account.runtime.email}"
}

resource "google_secret_manager_secret" "tokens" {
  secret_id = "adk-runtime-tokens"
  replication {
    auto {}
  }
}

resource "google_secret_manager_secret_version" "tokens" {
  secret      = google_secret_manager_secret.tokens.id
  secret_data = var.runtime_tokens
}

resource "google_secret_manager_secret_iam_member" "runtime_reads_tokens" {
  secret_id = google_secret_manager_secret.tokens.id
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:${google_service_account.runtime.email}"
}

resource "google_cloud_run_v2_service" "runtime" {
  name     = "adk-runtime"
  location = var.region
  ingress  = "INGRESS_TRAFFIC_ALL"

  template {
    service_account = google_service_account.runtime.email
    # A session is one request; this is the longest Cloud Run allows.
    timeout                          = "3600s"
    max_instance_request_concurrency = var.max_sessions
    # Reconnects from the same browser land on the same instance.
    session_affinity = true

    scaling {
      min_instance_count = var.min_instances
    }

    containers {
      image = var.image

      ports {
        name           = "http1"
        container_port = 8080
      }

      resources {
        limits = {
          cpu    = "2"
          memory = "1Gi"
        }
        # CPU always allocated: WebSocket sessions work between requests.
        cpu_idle          = false
        startup_cpu_boost = true
      }

      env {
        name  = "ADK_BUNDLES"
        value = "gs://${google_storage_bucket.bundles.name}/bundles"
      }
      env {
        name  = "ADK_SERVE"
        value = var.serve
      }
      env {
        name  = "ADK_MAX_SESSIONS"
        value = tostring(var.max_sessions)
      }
      # Cloud Run allows 10 seconds between SIGTERM and SIGKILL.
      env {
        name  = "ADK_DRAIN_SECS"
        value = "8"
      }
      env {
        name  = "GOOGLE_GENAI_USE_VERTEXAI"
        value = "true"
      }
      env {
        name  = "GOOGLE_CLOUD_PROJECT"
        value = var.project
      }
      env {
        name  = "GOOGLE_CLOUD_LOCATION"
        value = var.region
      }
      env {
        name = "ADK_RUNTIME_TOKENS"
        value_source {
          secret_key_ref {
            secret  = google_secret_manager_secret.tokens.secret_id
            version = "latest"
          }
        }
      }

      startup_probe {
        http_get {
          path = "/readyz"
        }
        period_seconds    = 2
        failure_threshold = 30
      }
      liveness_probe {
        http_get {
          path = "/healthz"
        }
        period_seconds = 15
      }
    }
  }

  depends_on = [
    google_project_iam_member.vertex,
    google_storage_bucket_iam_member.runtime_reads_bundles,
    google_secret_manager_secret_iam_member.runtime_reads_tokens,
    google_secret_manager_secret_version.tokens,
  ]
}

# Browsers and Twilio cannot present Google identity tokens, so the service
# is public at the Cloud Run layer and adk-runtime authenticates sessions.
resource "google_cloud_run_v2_service_iam_member" "public" {
  name     = google_cloud_run_v2_service.runtime.name
  location = google_cloud_run_v2_service.runtime.location
  role     = "roles/run.invoker"
  member   = "allUsers"
}

output "url" {
  description = "The service URL. Browsers connect to wss://.../ws/BUNDLE; Twilio posts to https://.../twilio/voice/BUNDLE."
  value       = google_cloud_run_v2_service.runtime.uri
}

output "bundles" {
  description = "ADK_BUNDLES for `adk bundle push`."
  value       = "gs://${google_storage_bucket.bundles.name}/bundles"
}
