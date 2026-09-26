# Authentication and Connecting

Every `Live::builder()` chain ends with a connect call. This chapter explains
the four connection methods, how credentials are resolved from the environment,
and how to diagnose missing-credential errors.

## The Recommended Path: `connect_from_env()`

The zero-ceremony entry point reads platform selection and credentials from
standard environment variables. You do not need to write token-fetching logic
for local development.

```rust,ignore
use gemini_adk_fluent_rs::prelude::*;

let handle = Live::builder()
    .voice(Voice::Kore)
    .instruction("You are a helpful voice assistant.")
    .connect_from_env()
    .await?;
```

Compare that to a typical pre-`connect_from_env()` bootstrap for Vertex AI:

```rust,ignore
// Without connect_from_env() -- roughly 30 lines in real code
let project = std::env::var("GOOGLE_CLOUD_PROJECT")?;
let location = std::env::var("GOOGLE_CLOUD_LOCATION")
    .unwrap_or_else(|_| "us-central1".to_string());
let token = if let Ok(tok) = std::env::var("GOOGLE_ACCESS_TOKEN") {
    tok
} else {
    let out = std::process::Command::new("gcloud")
        .args(["auth", "print-access-token"])
        .output()?;
    String::from_utf8(out.stdout)?.trim().to_string()
};

let handle = Live::builder()
    .connect_vertex(project, location, token)
    .await?;
```

The one-liner is equivalent and handles the gcloud fallback automatically.

## Platform Detection

`connect_from_env()` reads `GOOGLE_GENAI_USE_VERTEXAI` first:

| Value | Effect |
|-------|--------|
| `true` or `1` (case-insensitive) | Vertex AI mode |
| Unset, empty, or any other value | Google AI mode (default) |

## Environment Variable Resolution

### Google AI (default)

When `GOOGLE_GENAI_USE_VERTEXAI` is not `true`, the SDK checks for an API key
in this priority order:

1. `GEMINI_API_KEY`
2. `GOOGLE_GENAI_API_KEY`
3. `GOOGLE_API_KEY`

The first variable that is set and non-empty wins. All three names are accepted;
`GEMINI_API_KEY` is the canonical name for new projects.

### Vertex AI

When `GOOGLE_GENAI_USE_VERTEXAI=true`, the SDK reads:

| Variable | Required | Default |
|----------|----------|---------|
| `GOOGLE_CLOUD_PROJECT` | Yes | — |
| `GOOGLE_CLOUD_LOCATION` | No | `us-central1` |
| `GOOGLE_ACCESS_TOKEN` | No (see below) | metadata server, then gcloud |

If `GOOGLE_ACCESS_TOKEN` is unset or empty, `connect_from_env()` gets a token
from the metadata server when one answers (Cloud Run, GKE with Workload
Identity, Compute Engine), else by running `gcloud auth print-access-token`.
The token is refreshed in the background before it expires, so a reconnect
late in a long session still carries a valid one. This means the only
Vertex setup for local development is:

```sh
export GOOGLE_GENAI_USE_VERTEXAI=true
export GOOGLE_CLOUD_PROJECT=my-gcp-project
# No GOOGLE_ACCESS_TOKEN needed if gcloud is already authenticated
gcloud auth login   # once
```

On Cloud Run and GKE, attach a service account with `roles/aiplatform.user`
and set nothing else. A fixed `GOOGLE_ACCESS_TOKEN` is not refreshed, so
avoid it in long-running services.

The metadata server needs an HTTP client, which the `gemini-llm` (default)
and `gcs-store` features provide. A build with neither uses the gcloud CLI
only.

## Explicit Connection Methods

Use these when you need to pass credentials from a source other than the
environment, or when the platform is fixed at compile time.

### `connect_google_ai(api_key)`

```rust,ignore
let handle = Live::builder()
    .connect_google_ai(std::env::var("GEMINI_API_KEY")?)
    .await?;
```

The API key is appended to the WebSocket URL as `?key={api_key}`.

### `connect_vertex(project, location, access_token)`

```rust,ignore
let token = std::env::var("GOOGLE_ACCESS_TOKEN")?;

let handle = Live::builder()
    .connect_vertex("my-gcp-project", "us-central1", token)
    .await?;
```

The access token is sent as an `Authorization: Bearer {token}` header during
the WebSocket upgrade handshake.

#### Refreshing tokens

Vertex access tokens expire after about an hour, which is shorter than many
voice sessions plus their reconnects. `connect_vertex` accepts anything
`Into<AccessToken>`: a `&str`/`String` becomes `AccessToken::Static`, and
`AccessToken::from_fn(..)` is a `Dynamic` source consulted on every connection
attempt — including reconnects — so a reconnect never carries a stale
credential.

```rust,ignore
let handle = Live::builder()
    .connect_vertex(
        "my-gcp-project",
        "us-central1",
        AccessToken::from_fn(|| fetch_token_from_metadata_server()),
    )
    .await?;
```

At L0 the same thing is `ApiEndpoint::vertex_refreshing(project, location, || token())`
(and `SessionConfig::from_vertex(..)` / `ApiEndpoint::vertex(..)` take the same
`Into<AccessToken>`). `AccessToken`'s and `ApiEndpoint`'s `Debug` output never
show the token; `SessionConfig::bearer_token()` returns the current one as
`Option<String>` (it is `None` on Google AI).

### `connect(SessionConfig)`

For advanced scenarios — custom auth providers, private endpoints (VPC-SC),
or testing — build a `SessionConfig` directly and pass it to `.connect()`.
The builder merges the config's `endpoint` and `model` into its own settings,
preserving everything else configured on the `Live` builder (system
instruction, tools, voice, transcription, callbacks, and so on).

```rust,ignore
use gemini_genai_rs::prelude::*;

let config = SessionConfig::from_endpoint(
    ApiEndpoint::vertex_with_host(
        "my-project",
        "us-central1",
        token,
        "custom-vpc-endpoint.example.com",
    )
);

let handle = Live::builder()
    .instruction("You are a helpful assistant.")
    .on_audio(|data| { /* play audio */ })
    .connect(config)
    .await?;
```

## The L0 Building Block: `ApiEndpoint::from_env()`

`connect_from_env()` delegates credential resolution to
`ApiEndpoint::from_env()` from `gemini_genai_rs`. You can call this directly
when building `SessionConfig` at the L0 or L1 layer:

```rust,ignore
use gemini_genai_rs::protocol::types::ApiEndpoint;

let endpoint = ApiEndpoint::from_env()?;
let config = SessionConfig::from_endpoint(endpoint)
    .system_instruction("You are a helpful assistant.");
```

`from_env()` returns `Err(EndpointEnvError::Missing(var_name))` naming the
missing variable so the failure is immediately actionable. The `connect_from_env()`
Vertex AI path intercepts the `Missing("GOOGLE_ACCESS_TOKEN")` error specifically
and fetches a token from the metadata server or gcloud before propagating. Any other `EndpointEnvError`
is converted to an `AgentError::Config` with a diagnostic message.

## Troubleshooting Credential Errors

### Google AI: missing API key

```
Error: connect_from_env: missing required environment variable:
       GEMINI_API_KEY (or GOOGLE_GENAI_API_KEY / GOOGLE_API_KEY).
       For Google AI set GEMINI_API_KEY; ...
```

Set any one of `GEMINI_API_KEY`, `GOOGLE_GENAI_API_KEY`, or `GOOGLE_API_KEY`.

### Vertex AI: missing project

```
Error: GOOGLE_CLOUD_PROJECT is required for Vertex AI
```

Set `GOOGLE_CLOUD_PROJECT` to your GCP project ID and ensure
`GOOGLE_GENAI_USE_VERTEXAI=true`.

### Vertex AI: no credentials

```
Error: Vertex AI needs an access token: set GOOGLE_ACCESS_TOKEN, run on Google
       Cloud with a service account, or install the gcloud CLI (...)
```

Neither the metadata server nor gcloud answered. On a workstation, install
and authenticate the gcloud CLI (`gcloud auth login`). On Google Cloud, check
that the workload has a service account (on GKE, that Workload Identity is
set up for its Kubernetes service account). The part in parentheses says
what each source reported, including gcloud's own error when it is
installed but not logged in.

## Platform Differences Relevant to Auth

| Feature | Google AI | Vertex AI |
|---------|-----------|-----------|
| Credential type | API key (`?key=...`) | OAuth2 bearer token (header) |
| WebSocket host | `generativelanguage.googleapis.com` | `{location}-aiplatform.googleapis.com` |
| API version in path | `v1beta` | `v1beta1` |
| Frame format | Text WebSocket frames | Binary WebSocket frames (handled automatically) |
| Async tool calling | Supported | Not supported (fields stripped automatically) |
| Thinking config | Supported | Not supported (stripped automatically) |

The SDK handles Binary-frame decoding and field stripping transparently. You
can write the same agent code and switch platforms with a single env-var change.

## Next Steps

- [Live Sessions](./live-sessions.md) — full session configuration and the
  runtime `LiveHandle` API
- [Live Callbacks](./live-callbacks.md) — wiring fast-lane and control-lane
  event handlers
