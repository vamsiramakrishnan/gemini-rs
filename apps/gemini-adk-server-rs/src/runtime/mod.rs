//! `adk-runtime`: serve session-spec bundles in production.
//!
//! The runtime loads bundles by reference (`booking:prod`) from a
//! [bundle store](gemini_adk_fluent_rs::spec::store), and serves each one as
//! Live sessions over two entry points:
//!
//! | Route | For |
//! |---|---|
//! | `GET /ws/{bundle}` | browsers and apps: WebSocket, JSON text plus binary PCM |
//! | `POST /twilio/voice/{bundle}` | Twilio's voice webhook (signed), answers TwiML |
//! | `GET /twilio/media/{bundle}/{token}` | Twilio Media Streams WebSocket |
//! | `GET /healthz`, `GET /readyz` | liveness and readiness probes |
//! | `GET /metrics` | Prometheus text: sessions active, total, refused |
//! | `GET /v1/bundles` | what each reference resolves to now |
//!
//! Labels are re-resolved every `ADK_REFRESH_SECS` and on SIGHUP, so
//! moving `prod` to another version takes effect for new sessions without a
//! redeploy; a running session keeps the version it started with. On
//! SIGTERM the runtime stops admitting sessions (503, and `/readyz` fails),
//! lets running ones continue for `ADK_DRAIN_SECS`, then closes them.
//!
//! Specs run as written: tools bound to MCP servers and HTTP endpoints are
//! called. That is right for a store only the operator can write to, where
//! every version passed validation at push. A server that runs specs from
//! anyone else should pass them through
//! [`SessionSpec::sandboxed`](gemini_adk_fluent_rs::spec::SessionSpec::sandboxed)
//! first; this runtime does not.
//!
//! Configuration is by environment; see [`RuntimeConfig::from_env`] and
//! `docs/user-guide/deploy.md`.

mod auth;
mod bundles;
mod config;
mod twilio;
mod web;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, State as AxumState};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures::future::BoxFuture;
use tokio::sync::{Notify, watch};
use tracing::{info, warn};

use gemini_adk_fluent_rs::live::Live;
use gemini_adk_fluent_rs::spec::{BundleStore, SpecResources, open_store};
use gemini_adk_rs::State;
use gemini_adk_rs::error::AgentError;
use gemini_adk_rs::live::LiveHandle;
use gemini_adk_rs::llm::BaseLlm;

pub use auth::{SUBPROTOCOL, TOKEN_SUBPROTOCOL_PREFIX, twilio_signature, twilio_signature_valid};
pub use bundles::{LoadedBundle, RefreshReport};
pub use config::RuntimeConfig;

use bundles::BundleSet;

/// Connects a configured [`Live`] builder to a model. The default is
/// [`connect_from_env`]; tests pass one that plays a
/// [`ScriptedServer`](gemini_adk_fluent_rs::testing::ScriptedServer).
pub type Connector =
    Arc<dyn Fn(Live) -> BoxFuture<'static, Result<LiveHandle, AgentError>> + Send + Sync>;

/// A [`Connector`] that uses [`Live::connect_from_env`]: Google AI with an
/// API key, or Vertex AI with the service's own credentials.
pub fn connect_from_env() -> Connector {
    Arc::new(|live: Live| Box::pin(live.connect_from_env()))
}

/// After the grace period, how long closed sessions get to finish closing.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// Session counts, and a signal for when the last session ends.
#[derive(Default)]
struct Gauge {
    active: AtomicUsize,
    total: AtomicU64,
    refused_full: AtomicU64,
    refused_draining: AtomicU64,
    idle: Notify,
}

impl Gauge {
    /// Wait until no session is active or `deadline` passes; true if idle.
    async fn wait_idle(&self, deadline: tokio::time::Instant) -> bool {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.active.load(Ordering::SeqCst) == 0 {
                return true;
            }
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep_until(deadline) => {
                    return self.active.load(Ordering::SeqCst) == 0;
                }
            }
        }
    }
}

/// Resolves once `closing` turns true (or its sender is gone). Written as
/// its own future so the `watch::Ref` never lives across an await.
pub(crate) async fn closed(closing: &mut watch::Receiver<bool>) {
    let _ = closing.wait_for(|closed| *closed).await;
}

/// One admitted session. Dropping it frees the slot.
pub(crate) struct SessionPermit {
    gauge: Arc<Gauge>,
}

impl Drop for SessionPermit {
    fn drop(&mut self) {
        if self.gauge.active.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.gauge.idle.notify_waiters();
        }
    }
}

/// Why a session was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    Full,
    Draining,
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let body = match self {
            Refusal::Full => "at capacity: too many sessions on this instance",
            Refusal::Draining => "shutting down: not accepting new sessions",
        };
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "1")],
            body,
        )
            .into_response()
    }
}

/// The runtime server's state. See the [module docs](self).
pub struct Runtime {
    config: RuntimeConfig,
    bundles: BundleSet,
    gauge: Arc<Gauge>,
    draining: AtomicBool,
    closing: watch::Sender<bool>,
    connector: Connector,
    stream_tokens: Option<auth::StreamTokens>,
    extraction_llm: tokio::sync::OnceCell<Arc<dyn BaseLlm>>,
    refresh_now: Notify,
}

impl Runtime {
    /// A runtime for `config` over `store`, connecting sessions with
    /// [`connect_from_env`]. Nothing is loaded until
    /// [`refresh`](Self::refresh). Fails when the configuration does not
    /// pass [`RuntimeConfig::check`].
    pub fn new(config: RuntimeConfig, store: Arc<dyn BundleStore>) -> Result<Self, String> {
        config.check()?;
        Ok(Self {
            bundles: BundleSet::new(store, &config.serve),
            stream_tokens: config
                .twilio_auth_token
                .as_deref()
                .map(auth::StreamTokens::new),
            config,
            gauge: Arc::new(Gauge::default()),
            draining: AtomicBool::new(false),
            closing: watch::Sender::new(false),
            connector: connect_from_env(),
            extraction_llm: tokio::sync::OnceCell::new(),
            refresh_now: Notify::new(),
        })
    }

    /// Connect sessions with `connector` instead.
    pub fn with_connector(mut self, connector: Connector) -> Self {
        self.connector = connector;
        self
    }

    /// Back specs that declare `extract` with `llm`, instead of a
    /// `GeminiLlm` configured from the environment.
    pub fn with_extraction_llm(self, llm: Arc<dyn BaseLlm>) -> Self {
        let _ = self.extraction_llm.set(llm);
        self
    }

    /// The configuration.
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// Resolve every served reference again. New sessions use a version
    /// that changed; running sessions keep theirs. A reference that fails
    /// keeps the version it had.
    pub async fn refresh(&self) -> RefreshReport {
        let report = self.bundles.refresh().await;
        for (reference, old, new) in &report.changed {
            match old {
                Some(old) => info!("{reference} now resolves to {new} (was {old})"),
                None => info!("{reference} loaded at {new}"),
            }
        }
        for error in &report.errors {
            warn!("bundle refresh: {error}");
        }
        report
    }

    /// Ask the refresh loop to refresh now (what SIGHUP does).
    pub fn request_refresh(&self) {
        self.refresh_now.notify_one();
    }

    /// The bundle served under `name`, as new sessions would get it.
    pub fn bundle(&self, name: &str) -> Option<Arc<LoadedBundle>> {
        self.bundles.get(name)
    }

    /// `Ok` when every bundle is loaded, the store is reachable and the
    /// runtime is not draining; otherwise the reasons.
    pub fn readiness(&self) -> Result<(), Vec<String>> {
        let mut reasons = self.bundles.not_ready_reasons();
        if self.is_draining() {
            reasons.push("draining".into());
        }
        if reasons.is_empty() {
            Ok(())
        } else {
            Err(reasons)
        }
    }

    /// Sessions running now.
    pub fn active_sessions(&self) -> usize {
        self.gauge.active.load(Ordering::SeqCst)
    }

    /// Whether the runtime has stopped admitting sessions.
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// Stop admitting sessions. New ones get 503 and `/readyz` fails;
    /// running ones continue.
    pub fn begin_drain(&self) {
        if !self.draining.swap(true, Ordering::SeqCst) {
            info!(
                active = self.active_sessions(),
                "draining: refusing new sessions"
            );
        }
    }

    /// Drain: stop admitting sessions, wait up to `drain_grace` for running
    /// ones to end, then close the rest. Returns how many had to be closed.
    pub async fn drain(&self) -> usize {
        self.begin_drain();
        let deadline = tokio::time::Instant::now() + self.config.drain_grace;
        if self.gauge.wait_idle(deadline).await {
            return 0;
        }
        let remaining = self.active_sessions();
        info!("grace period over: closing {remaining} session(s)");
        self.closing.send_replace(true);
        self.gauge
            .wait_idle(tokio::time::Instant::now() + CLOSE_WAIT)
            .await;
        remaining
    }

    /// Re-resolve labels every `refresh_every`, and whenever
    /// [`request_refresh`](Self::request_refresh) is called, until the
    /// runtime closes its sessions.
    pub fn spawn_refresh_loop(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            let mut closing = runtime.closing.subscribe();
            loop {
                tokio::select! {
                    () = tokio::time::sleep(runtime.config.refresh_every) => {}
                    () = runtime.refresh_now.notified() => {}
                    () = closed(&mut closing) => break,
                }
                runtime.refresh().await;
            }
        })
    }

    /// The HTTP routes. See the [module docs](self).
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/healthz", get(healthz))
            .route("/readyz", get(readyz))
            .route("/metrics", get(metrics))
            .route("/v1/bundles", get(list_bundles))
            .route("/ws/{bundle}", get(web::session))
            .route("/twilio/voice/{bundle}", post(twilio::voice))
            .route("/twilio/media/{bundle}/{token}", get(twilio::media))
            // Webhook bodies are small forms; nothing else takes a body.
            .layer(DefaultBodyLimit::max(64 * 1024))
            .with_state(Arc::clone(self))
    }

    /// Serve on `listener` until `shutdown` completes, then
    /// [`drain`](Self::drain) and stop.
    pub async fn serve(
        self: Arc<Self>,
        listener: tokio::net::TcpListener,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        let app = self.router();
        let runtime = Arc::clone(&self);
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown.await;
                runtime.drain().await;
            })
            .await
    }

    /// Admit one session, or say why not.
    pub(crate) fn admit(&self) -> Result<SessionPermit, Refusal> {
        if self.is_draining() {
            self.gauge.refused_draining.fetch_add(1, Ordering::Relaxed);
            return Err(Refusal::Draining);
        }
        let max = self.config.max_sessions;
        match self
            .gauge
            .active
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < max).then_some(n + 1)
            }) {
            Ok(_) => {
                self.gauge.total.fetch_add(1, Ordering::Relaxed);
                Ok(SessionPermit {
                    gauge: Arc::clone(&self.gauge),
                })
            }
            Err(_) => {
                self.gauge.refused_full.fetch_add(1, Ordering::Relaxed);
                Err(Refusal::Full)
            }
        }
    }

    /// A receiver that turns true when running sessions must close.
    pub(crate) fn closing(&self) -> watch::Receiver<bool> {
        self.closing.subscribe()
    }

    /// Whether a client presented an accepted token (or none is needed).
    pub(crate) fn client_allowed(&self, headers: &HeaderMap) -> bool {
        self.config.open_sessions() || auth::authorized(headers, &self.config.tokens)
    }

    /// Build a session from `bundle` and connect it.
    pub(crate) async fn connect(&self, bundle: &LoadedBundle) -> Result<LiveHandle, String> {
        let mut resources = SpecResources::default();
        if !bundle.spec.extract.is_empty() {
            resources.extraction_llm = Some(self.extraction_llm().await?);
        }
        let mut live = Live::builder();
        if let Some(model) = &self.config.model {
            live = live.model(gemini_adk_fluent_rs::prelude::ModelId::new(model.clone()));
        }
        let live = bundle.spec.apply(live, &State::new(), &resources)?;
        (self.connector)(live).await.map_err(|e| e.to_string())
    }

    async fn extraction_llm(&self) -> Result<Arc<dyn BaseLlm>, String> {
        self.extraction_llm
            .get_or_try_init(build_extraction_llm)
            .await
            .cloned()
    }

    fn metrics_text(&self) -> String {
        let g = &self.gauge;
        let mut out = String::new();
        let mut metric = |name: &str, kind: &str, help: &str, lines: Vec<String>| {
            out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
            for line in lines {
                out.push_str(&line);
                out.push('\n');
            }
        };
        metric(
            "adk_runtime_sessions_active",
            "gauge",
            "Sessions running now.",
            vec![format!(
                "adk_runtime_sessions_active {}",
                g.active.load(Ordering::SeqCst)
            )],
        );
        metric(
            "adk_runtime_sessions_total",
            "counter",
            "Sessions admitted since start.",
            vec![format!(
                "adk_runtime_sessions_total {}",
                g.total.load(Ordering::Relaxed)
            )],
        );
        metric(
            "adk_runtime_sessions_refused_total",
            "counter",
            "Sessions refused with 503, by reason.",
            vec![
                format!(
                    "adk_runtime_sessions_refused_total{{reason=\"full\"}} {}",
                    g.refused_full.load(Ordering::Relaxed)
                ),
                format!(
                    "adk_runtime_sessions_refused_total{{reason=\"draining\"}} {}",
                    g.refused_draining.load(Ordering::Relaxed)
                ),
            ],
        );
        metric(
            "adk_runtime_draining",
            "gauge",
            "1 while the runtime refuses new sessions.",
            vec![format!(
                "adk_runtime_draining {}",
                u8::from(self.is_draining())
            )],
        );
        metric(
            "adk_runtime_bundle_info",
            "gauge",
            "The version each served reference resolves to.",
            self.bundles
                .loaded()
                .iter()
                .map(|b| {
                    format!(
                        "adk_runtime_bundle_info{{bundle=\"{}\",reference=\"{}\",version=\"{}\"}} 1",
                        b.name, b.reference, b.version.version
                    )
                })
                .collect(),
        );
        out
    }
}

/// A `GeminiLlm` for `extract` pipelines, from the same environment as the
/// Live session. On Vertex without `GOOGLE_ACCESS_TOKEN`, its token comes
/// from the metadata server (or gcloud) and is kept fresh.
async fn build_extraction_llm() -> Result<Arc<dyn BaseLlm>, String> {
    use gemini_adk_fluent_rs::gemini_genai_rs::protocol::types::AccessToken;
    use gemini_adk_fluent_rs::gemini_genai_rs::transport::auth::GoogleAccessToken;
    use gemini_adk_rs::llm::{GeminiLlm, GeminiLlmParams, TokenProvider};

    struct Refreshing(AccessToken);
    impl TokenProvider for Refreshing {
        fn token(&self) -> String {
            self.0.get()
        }
    }

    let env = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());
    let vertex = env("GOOGLE_GENAI_USE_VERTEXAI").is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let mut params = GeminiLlmParams {
        model: env("GEMINI_EXTRACTION_MODEL"),
        ..GeminiLlmParams::default()
    };
    if vertex && env("GOOGLE_ACCESS_TOKEN").is_none() {
        let token = GoogleAccessToken::from_env()
            .into_access_token()
            .await
            .map_err(|e| format!("extraction model credentials: {e}"))?;
        params.token_provider = Some(Arc::new(Refreshing(token)));
    }
    Ok(Arc::new(GeminiLlm::new(params)))
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "a valid token is required: Authorization: Bearer <token>, X-API-Key, \
         or the WebSocket subprotocol adk.token.<token>",
    )
        .into_response()
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(AxumState(runtime): AxumState<Arc<Runtime>>) -> Response {
    match runtime.readiness() {
        Ok(()) => (StatusCode::OK, "ready").into_response(),
        Err(reasons) => (StatusCode::SERVICE_UNAVAILABLE, reasons.join("\n")).into_response(),
    }
}

async fn metrics(AxumState(runtime): AxumState<Arc<Runtime>>, headers: HeaderMap) -> Response {
    if !runtime.client_allowed(&headers) {
        return unauthorized();
    }
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        runtime.metrics_text(),
    )
        .into_response()
}

async fn list_bundles(AxumState(runtime): AxumState<Arc<Runtime>>, headers: HeaderMap) -> Response {
    if !runtime.client_allowed(&headers) {
        return unauthorized();
    }
    let bundles: Vec<serde_json::Value> = runtime
        .bundles
        .loaded()
        .iter()
        .map(|b| {
            serde_json::json!({
                "name": b.name,
                "reference": b.reference,
                "version": b.version,
                "modality": b.spec.modality,
            })
        })
        .collect();
    axum::Json(serde_json::json!({ "bundles": bundles })).into_response()
}

/// Run `adk-runtime` from the environment: load every bundle (failing if
/// one cannot be loaded), serve on `0.0.0.0:$PORT`, re-resolve labels
/// periodically and on SIGHUP, and drain on SIGTERM or Ctrl-C.
pub async fn run_from_env() -> Result<(), String> {
    let config = RuntimeConfig::from_env()?;
    if config.open_sessions() {
        warn!("ADK_RUNTIME_INSECURE=1 and no ADK_RUNTIME_TOKENS: session endpoints are open");
    }
    let store = open_store(&config.store).map_err(|e| format!("ADK_BUNDLES: {e}"))?;
    let runtime = Arc::new(Runtime::new(config, store)?);
    let report = runtime.refresh().await;
    if !report.errors.is_empty() {
        return Err(format!(
            "could not load every bundle in ADK_SERVE: {}",
            report.errors.join("; ")
        ));
    }
    runtime.spawn_refresh_loop();
    spawn_sighup(&runtime);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], runtime.config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    info!(
        "adk-runtime listening on {addr}, serving {}",
        runtime.config.serve.join(", ")
    );
    runtime
        .serve(listener, shutdown_signal())
        .await
        .map_err(|e| e.to_string())?;
    info!("adk-runtime stopped");
    Ok(())
}

#[cfg(unix)]
fn spawn_sighup(runtime: &Arc<Runtime>) {
    use tokio::signal::unix::{SignalKind, signal};
    let runtime = Arc::clone(runtime);
    tokio::spawn(async move {
        let Ok(mut hup) = signal(SignalKind::hangup()) else {
            warn!("SIGHUP handler unavailable; labels refresh on the interval only");
            return;
        };
        while hup.recv().await.is_some() {
            info!("SIGHUP: refreshing bundles");
            runtime.request_refresh();
        }
    });
}

#[cfg(not(unix))]
fn spawn_sighup(_runtime: &Arc<Runtime>) {}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = term.recv() => info!("SIGTERM: draining"),
                    _ = tokio::signal::ctrl_c() => info!("interrupt: draining"),
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        info!("interrupt: draining");
    }
}
