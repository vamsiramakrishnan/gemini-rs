variable "project" {
  description = "Google Cloud project ID."
  type        = string
}

variable "region" {
  description = "Region for the service and the bundle bucket."
  type        = string
  default     = "us-central1"
}

variable "image" {
  description = "The adk-runtime image, built from deploy/Dockerfile (for example REGION-docker.pkg.dev/PROJECT/adk/adk-runtime:TAG)."
  type        = string
}

variable "bucket" {
  description = "Name of the bucket to create for bundles. Bundles live under gs://BUCKET/bundles."
  type        = string
}

variable "serve" {
  description = "ADK_SERVE: comma-separated bundle references, such as booking:prod."
  type        = string
}

variable "runtime_tokens" {
  description = "ADK_RUNTIME_TOKENS: comma-separated tokens clients present. Stored in Secret Manager, and in Terraform state."
  type        = string
  sensitive   = true
}

variable "max_sessions" {
  description = "Concurrent sessions per instance (ADK_MAX_SESSIONS and the Cloud Run request concurrency)."
  type        = number
  default     = 50
}

variable "min_instances" {
  description = "Instances kept warm."
  type        = number
  default     = 1
}
