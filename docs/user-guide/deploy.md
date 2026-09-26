# Deploying the runtime

`adk-runtime` serves [bundles](./bundles.md) in production. It loads each
bundle by reference, such as `booking:prod`, from a bundle store, and runs
every session from that spec. Promoting or rolling back means moving a label
in the store. The runtime picks the change up for new sessions without a
redeploy, and a running session keeps the version it started with.

The service image is the same whatever it serves. What runs comes from the
store.

| Route | Purpose |
|---|---|
| `GET /ws/{bundle}` | WebSocket session for browsers and apps (token required) |
| `POST /twilio/voice/{bundle}` | Twilio voice webhook (signature checked); answers TwiML |
| `GET /twilio/media/{bundle}/{token}` | Twilio Media Streams WebSocket |
| `GET /healthz` | Liveness: the process is up |
| `GET /readyz` | Readiness: every bundle loaded, store reachable, not draining |
| `GET /metrics` | Prometheus text (token required) |
| `GET /v1/bundles` | The version each reference resolves to now (token required) |

## Run it locally

Push a spec to a directory store, label it, and start the runtime:

```bash
adk bundle push agent.json --store ./bundles --name booking --label prod

export GEMINI_API_KEY=...            # or Vertex AI, see below
ADK_BUNDLES=./bundles ADK_SERVE=booking:prod ADK_RUNTIME_TOKENS=dev-token \
  cargo run --release -p gemini-adk-server-rs --bin adk-runtime
```

It listens on `0.0.0.0:8080` (set `PORT` to change it). Check it:

```bash
curl localhost:8080/readyz
curl -H 'Authorization: Bearer dev-token' localhost:8080/v1/bundles
```

A browser cannot set headers on a WebSocket, so it sends the token as a
subprotocol:

```js
const ws = new WebSocket("ws://localhost:8080/ws/booking", ["adk.v1", "adk.token.dev-token"]);
ws.onopen = () => {
  ws.send(JSON.stringify({ type: "start" }));
  ws.send(JSON.stringify({ type: "text", text: "I'd like a table for two." }));
};
ws.onmessage = (event) => console.log(event.data);
```

The messages are the ones the web app uses. The client sends `start`, then
`text` turns and microphone audio. Audio is 16 kHz mono PCM16, either as
binary frames or base64 in `{"type": "audio", "data": ...}`. `stop` ends the
session. The server sends `connected`, then a `stateUpdate` whose key is
`runtime:bundle` and whose value names the version, then `textDelta`,
`textComplete`, `inputTranscription`, `outputTranscription`, `turnComplete`,
`interrupted` and `error` messages. Model audio arrives as binary frames of
24 kHz mono PCM16. Session state and tool calls stay on the server. A
`start` message's instruction, model and voice overrides are ignored, since
the spec comes from the store.

## Configuration

Everything is set by environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `ADK_BUNDLES` | required | Bundle store: a directory or `gs://bucket/prefix` |
| `ADK_SERVE` | required | Comma-separated references, e.g. `booking:prod,clinic:prod` |
| `ADK_RUNTIME_TOKENS` | none | Comma-separated tokens clients present |
| `ADK_RUNTIME_INSECURE` | off | `1` serves session endpoints without a token |
| `TWILIO_AUTH_TOKEN` | none | Enables the Twilio routes |
| `ADK_PUBLIC_URL` | from request headers | Public base URL, e.g. `https://booking-abc.a.run.app` |
| `PORT` | `8080` | Listen port, on every interface |
| `ADK_MAX_SESSIONS` | `50` | Concurrent sessions; more get 503 |
| `ADK_MAX_SESSION_SECS` | `3600` | A session is closed after this long |
| `ADK_MAX_MESSAGE_BYTES` | `262144` | Largest WebSocket message accepted |
| `ADK_REFRESH_SECS` | `30` | How often labels are re-resolved |
| `ADK_DRAIN_SECS` | `8` | How long sessions may continue after SIGTERM |
| `GEMINI_LIVE_MODEL` | platform default | Live model for every session |
| `GEMINI_EXTRACTION_MODEL` | SDK default | Model for specs that declare `extract` |

Model credentials are the usual ones (see
[authentication](./auth-and-connecting.md)). For Google AI, set
`GEMINI_API_KEY`. For Vertex AI, set `GOOGLE_GENAI_USE_VERTEXAI=true`,
`GOOGLE_CLOUD_PROJECT` and `GOOGLE_CLOUD_LOCATION`. On Cloud Run and GKE the
token then comes from the service account through the metadata server, and
is refreshed before it expires. The same credentials read a `gs://` store.

## Bundles and labels

`ADK_SERVE` lists what to serve. Each entry is `name`, `name:label` or
`name@version`, and the name is the route: `booking:prod` is served at
`/ws/booking`. A name can appear once.

At startup the runtime resolves every reference. If one does not resolve,
it exits with an error, so a deploy with a typo never takes traffic. After
that it re-resolves every `ADK_REFRESH_SECS` seconds, and at once on
`SIGHUP`. When a label has moved, new sessions get the new version, and the
log says `booking:prod now resolves to 3f2a9c1d4e5f (was 1a2b3c4d5e6f)`. When
a refresh fails, for example because the store is unreachable, the runtime
keeps serving the version it has. `/readyz` reports not ready after three
failed refreshes in a row.

Specs from the store run as written. Tools bound to MCP servers and HTTP
endpoints are called for real. That is right for a store only you can
write to, where every version passed validation when it was pushed. Keep
write access to the bucket to the people and CI jobs that release agents.
The runtime reads with `roles/storage.objectViewer`. A server that runs
specs written by someone else must pass them through
`SessionSpec::sandboxed` first, as the Flow Studio does; `adk-runtime` does
not.

An MCP entry that starts a local command needs that command in the image.
A spec that declares `memory` is refused at load, because the runtime has no
memory engine. Serve such a spec from your own server with
`SpecResources::memory`.

## Authentication

With neither `ADK_RUNTIME_TOKENS` nor `TWILIO_AUTH_TOKEN` set, the runtime
refuses to start. `ADK_RUNTIME_INSECURE=1` overrides that for local
development only.

Session endpoints, `/metrics` and `/v1/bundles` accept a token from
`ADK_RUNTIME_TOKENS` in any of three places:

- `Authorization: Bearer <token>`
- `X-API-Key: <token>`
- a `Sec-WebSocket-Protocol` entry `adk.token.<token>`, next to `adk.v1`

Give each client its own token, so you can revoke one by removing it from
the list and redeploying. Use letters, digits, `-`, `_` and `.` only
(`openssl rand -hex 32` is fine): a browser rejects a subprotocol with other
characters. `/healthz` and `/readyz` are open, so probes need
no credentials.

Twilio cannot send custom headers, so phone calls are authenticated in two
steps:

1. Twilio's webhook request carries `X-Twilio-Signature`: an HMAC-SHA1,
   keyed with your account's auth token, over the URL Twilio called and the
   form parameters. The runtime recomputes it with `TWILIO_AUTH_TOKEN` and
   refuses the request (403) if it differs.
2. The TwiML answer connects the call to
   `wss://<host>/twilio/media/{bundle}/{token}`. The token expires after 60
   seconds and is bound to the bundle and to the call's SID. The SID is not
   in the URL: the runtime checks it against the stream's `start` frame
   before starting a Live session, and accepts each token once.

The signed URL must be the one Twilio called. Behind a proxy that rewrites
the host, set `ADK_PUBLIC_URL`. Otherwise the runtime rebuilds the URL from
`X-Forwarded-Proto` and `Host`, which is correct on Cloud Run.

## Phone calls

Set `TWILIO_AUTH_TOKEN`, then in the Twilio console set the number's voice
webhook to `https://<service>/twilio/voice/booking` (HTTP POST). The bundle
must be an audio spec. Each call gets its own session, bridged with
`TwilioCall` as described in [Telephony](./telephony.md). DTMF digits land in
session state under the `telephony:` keys.

When the instance is full or draining, the webhook answers with a busy
signal (`<Reject reason="busy"/>`) rather than an error.

SIP is not served by `adk-runtime`. A SIP agent (see `examples/sip-agent`)
needs SIP signalling and a range of UDP ports for RTP, which Cloud Run cannot
provide. On GKE, run it as a separate Deployment with `hostNetwork: true` on
a node pool with public IPs, and open the SIP and RTP ports in the VPC
firewall.

## Limits, probes and shutdown

- More than `ADK_MAX_SESSIONS` concurrent sessions get `503` with
  `Retry-After: 1`. Admission happens before the WebSocket upgrade, so a
  refused client sees an HTTP status.
- A session that reaches `ADK_MAX_SESSION_SECS` gets an `error` message,
  `session time limit reached`, and is closed. A client that does not send
  `start` within 10 seconds is closed.
- A WebSocket message larger than `ADK_MAX_MESSAGE_BYTES` closes the
  connection.
- `/metrics` reports `adk_runtime_sessions_active`,
  `adk_runtime_sessions_total`, `adk_runtime_sessions_refused_total` by
  reason, `adk_runtime_draining` and `adk_runtime_bundle_info`.

On `SIGTERM` (or Ctrl-C) the runtime drains. `/readyz` fails, new sessions
get 503, and running sessions continue for up to `ADK_DRAIN_SECS`. Sessions
still open then get an `error` message, `server is shutting down`, and are
closed. The process exits once they have closed. Clients should reconnect on
503 or on that message; a retry reaches another instance.

## Cloud Run

`adk deploy` builds the image with Cloud Build and deploys it. Run it from a
checkout of this repository:

```bash
gcloud services enable run.googleapis.com cloudbuild.googleapis.com \
  artifactregistry.googleapis.com aiplatform.googleapis.com secretmanager.googleapis.com
gcloud artifacts repositories create adk --repository-format docker --location us-central1
printf 'client-a-token,client-b-token' | gcloud secrets create adk-runtime-tokens --data-file=-

adk deploy cloud-run --project my-project \
  --bundles gs://my-bucket/bundles --serve booking:prod
```

It prints each command before running it. `--dry-run` prints them without
running, and `--image` deploys an image you built instead. It expects a
service account `adk-runtime@PROJECT.iam.gserviceaccount.com` with
`roles/aiplatform.user`, `roles/storage.objectViewer` on the bucket, and
`roles/secretmanager.secretAccessor` on the secrets. `--service-account`
names another one. `--twilio-secret` adds `TWILIO_AUTH_TOKEN` from a secret.

The same deployment is in
[`deploy/cloudrun/service.yaml`](../../deploy/cloudrun/service.yaml), for
`gcloud run services replace`, and in
[`deploy/terraform`](../../deploy/terraform/main.tf). The Terraform also
creates the service account, its roles, the bucket and the secret. The
settings that matter for voice sessions:

- **Timeout 3600 s.** A WebSocket is one request for its whole life, and
  Cloud Run closes it at the request timeout. One hour is the maximum.
- **CPU always allocated** (`--no-cpu-throttling`). With request-based
  allocation, CPU is throttled between client messages, and a voice session
  does its work there: model audio, tool calls, label refreshes.
- **Concurrency equal to `ADK_MAX_SESSIONS`.** Cloud Run then starts another
  instance before this one has to refuse.
- **Session affinity**, so a browser that reconnects lands on the same
  instance.
- **At least one instance warm**, so the first caller does not wait for a
  cold start.
- **`ADK_DRAIN_SECS=8`.** Cloud Run kills the container 10 seconds after
  `SIGTERM`, and the Cloud Run value cannot be raised.
- **Public invoker.** Browsers and Twilio cannot present Google identity
  tokens, so the service allows unauthenticated invocation and the runtime
  checks every session itself.

The image is built by [`deploy/Dockerfile`](../../deploy/Dockerfile) from
the repository root. It holds `adk-runtime` and `adk` on Debian slim with
the system CA certificates and OpenSSL. To build it locally:

```bash
docker build -f deploy/Dockerfile -t adk-runtime .
```

## GKE

The manifests are in [`deploy/gke`](../../deploy/gke/deployment.yaml). Replace
`PROJECT_ID`, `REGION`, `BUCKET` and the host, then:

```bash
gcloud iam service-accounts add-iam-policy-binding \
  adk-runtime@PROJECT_ID.iam.gserviceaccount.com \
  --role roles/iam.workloadIdentityUser \
  --member "serviceAccount:PROJECT_ID.svc.id.goog[default/adk-runtime]"
kubectl create secret generic adk-runtime-tokens --from-literal=tokens=client-a-token
kubectl apply -f deploy/gke/
```

`adk deploy gke --project my-project` builds and pushes the image, then
prints these steps. What the manifests set:

- Credentials come from Workload Identity. The Kubernetes service account
  maps to the Google one, so no key is stored in the cluster.
- `readinessProbe` is `/readyz` and `livenessProbe` is `/healthz`. A pod
  that is draining, or whose bundles are not loaded, gets no new traffic.
- `terminationGracePeriodSeconds: 60` with `ADK_DRAIN_SECS=50`. A rollout
  or scale-down gives running calls 50 seconds to finish.
- A `BackendConfig` with `timeoutSec: 3600`. The load balancer's default of
  30 seconds would cut every WebSocket.
- An Ingress with a Google-managed certificate, because browsers and Twilio
  need `wss://`.
- An HPA on CPU (2 to 10 pods) and a PodDisruptionBudget that lets node
  maintenance take one pod at a time.

## Promotion and rollback

Release a new version by pushing it and moving the label:

```bash
adk bundle push agent.json --store gs://my-bucket/bundles --name booking --label staging
# test against a runtime that serves booking:staging, then:
adk bundle list booking --store gs://my-bucket/bundles
adk bundle label booking prod 3f2a9c --store gs://my-bucket/bundles
```

Every runtime serving `booking:prod` switches within `ADK_REFRESH_SECS`.
Send `SIGHUP` to switch one at once. Sessions already running finish on the
version they started with. To roll back, point the label at the previous
version:

```bash
adk bundle label booking prod 1a2b3c --store gs://my-bucket/bundles
```

To confirm what an instance serves, read `/v1/bundles` or the
`adk_runtime_bundle_info` metric. Each session also reports its version in
its first `stateUpdate`.

To pin a deployment to one version, serve `booking@3f2a9c1d4e5f` instead of
a label. Moving labels then has no effect on it.
