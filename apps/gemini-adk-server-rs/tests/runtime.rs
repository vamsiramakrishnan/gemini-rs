//! `adk-runtime`, model-free: sessions connect to a scripted Live server,
//! bundles live in a temporary directory store.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
use tower::ServiceExt;

use gemini_adk_fluent_rs::gemini_genai_rs::transport::ReplayControl;
use gemini_adk_fluent_rs::live::Live;
use gemini_adk_fluent_rs::spec::{BundleStore, SessionSpec, open_store};
use gemini_adk_fluent_rs::testing::ScriptedServer;
use gemini_adk_server_rs::runtime::{Connector, Runtime, RuntimeConfig, twilio_signature};

const TOKEN: &str = "test-token";

struct Fixture {
    dir: std::path::PathBuf,
    store: Arc<dyn BundleStore>,
    /// Scripts waiting for their session to subscribe: `release()` them.
    controls: Arc<Mutex<Vec<ReplayControl>>>,
    connects: Arc<AtomicUsize>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("adk-runtime-test-{}", uuid::Uuid::new_v4()));
        let store = open_store(dir.to_str().unwrap()).unwrap();
        Self {
            dir,
            store,
            controls: Arc::default(),
            connects: Arc::default(),
        }
    }

    fn config(&self) -> RuntimeConfig {
        let mut config = RuntimeConfig::new(self.dir.to_str().unwrap(), ["booking:prod"]);
        config.tokens = vec![TOKEN.into()];
        config
    }

    /// Every session plays a script in which the model says "hello".
    fn connector(&self) -> Connector {
        let controls = Arc::clone(&self.controls);
        let connects = Arc::clone(&self.connects);
        Arc::new(move |live: Live| {
            let controls = Arc::clone(&controls);
            let connects = Arc::clone(&connects);
            Box::pin(async move {
                connects.fetch_add(1, Ordering::SeqCst);
                let (transport, control) = ScriptedServer::new().says("hello").into_transport();
                let handle = live.connect_with_transport(transport).await?;
                controls.lock().push(control);
                Ok(handle)
            })
        })
    }

    fn runtime(&self, config: RuntimeConfig) -> Arc<Runtime> {
        Arc::new(
            Runtime::new(config, Arc::clone(&self.store))
                .unwrap()
                .with_connector(self.connector()),
        )
    }

    /// Push a spec for `booking` and point `prod` at it; returns the version.
    async fn publish(&self, instruction: &str, modality: &str) -> String {
        let spec = SessionSpec::from_value(json!({
            "name": "booking",
            "instruction": instruction,
            "modality": modality,
            "flow": {"steps": [{"id": "serve", "terminal": true}]},
        }))
        .unwrap();
        let version = self.store.push("booking", &spec, None).await.unwrap();
        self.store
            .set_label("booking", "prod", &version.version)
            .await
            .unwrap();
        version.version
    }

    fn release_scripts(&self) {
        for control in self.controls.lock().iter() {
            control.release();
        }
    }
}

async fn serve(
    runtime: &Arc<Runtime>,
) -> (
    SocketAddr,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let runtime = Arc::clone(runtime);
    let task = tokio::spawn(async move {
        runtime
            .serve(listener, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    (addr, stop, task)
}

async fn get(runtime: &Arc<Runtime>, path: &str, headers: &[(&str, &str)]) -> (StatusCode, String) {
    let mut request = Request::builder().uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = runtime
        .router()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Open `/ws/booking` the way a browser does: the token in the subprotocol.
async fn open(addr: SocketAddr, token: &str) -> Result<Client, tungstenite::Error> {
    let mut request = format!("ws://{addr}/ws/booking")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "sec-websocket-protocol",
        format!("adk.v1, adk.token.{token}").parse().unwrap(),
    );
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(ws, _)| ws)
}

fn http_status(result: Result<Client, tungstenite::Error>) -> StatusCode {
    match result {
        Err(tungstenite::Error::Http(response)) => {
            StatusCode::from_u16(response.status().as_u16()).unwrap()
        }
        Err(e) => panic!("expected an HTTP refusal, got {e}"),
        Ok(_) => panic!("expected an HTTP refusal, the socket opened"),
    }
}

/// The next JSON message, skipping audio and control frames.
async fn next_json(ws: &mut Client) -> Option<Value> {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .ok()??
        {
            Ok(tungstenite::Message::Text(text)) => return serde_json::from_str(&text).ok(),
            Ok(tungstenite::Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

/// Send `start` and return the version the session reports.
async fn start(ws: &mut Client) -> String {
    ws.send(tungstenite::Message::Text(
        json!({"type": "start"}).to_string(),
    ))
    .await
    .unwrap();
    assert_eq!(next_json(ws).await.unwrap()["type"], "connected");
    let hello = next_json(ws).await.unwrap();
    assert_eq!(hello["key"], "runtime:bundle", "{hello}");
    hello["value"]["version"].as_str().unwrap().to_string()
}

async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn probes_are_open_and_session_endpoints_need_a_token() {
    let fx = Fixture::new();
    fx.publish("Book tables.", "text").await;
    let runtime = fx.runtime(fx.config());
    runtime.refresh().await;

    assert_eq!(get(&runtime, "/healthz", &[]).await.0, StatusCode::OK);
    assert_eq!(get(&runtime, "/readyz", &[]).await.0, StatusCode::OK);

    assert_eq!(
        get(&runtime, "/ws/booking", &[]).await.0,
        StatusCode::UNAUTHORIZED
    );
    let wrong = [("authorization", "Bearer nope")];
    assert_eq!(
        get(&runtime, "/ws/booking", &wrong).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&runtime, "/metrics", &[]).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&runtime, "/v1/bundles", &[]).await.0,
        StatusCode::UNAUTHORIZED
    );

    // A good token gets past authentication; this plain GET then fails the
    // WebSocket upgrade instead.
    let (status, _) = get(&runtime, "/ws/booking", &[("x-api-key", TOKEN)]).await;
    assert!(
        status.is_client_error() && status != StatusCode::UNAUTHORIZED,
        "{status}"
    );
    let bearer = format!("Bearer {TOKEN}");
    let (status, body) = get(&runtime, "/metrics", &[("authorization", &bearer)]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("adk_runtime_sessions_active 0"), "{body}");
    assert!(
        body.contains("adk_runtime_bundle_info{bundle=\"booking\""),
        "{body}"
    );
    let (status, _) = get(&runtime, "/ws/unknown", &[("authorization", &bearer)]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Over a real socket: refused without the token, served with it.
    let (addr, _stop, _task) = serve(&runtime).await;
    assert_eq!(
        http_status(open(addr, "nope").await),
        StatusCode::UNAUTHORIZED
    );
    let mut ws = open(addr, TOKEN).await.unwrap();
    start(&mut ws).await;
}

#[tokio::test]
async fn insecure_mode_serves_without_a_token() {
    let fx = Fixture::new();
    fx.publish("Book tables.", "text").await;
    let mut config = fx.config();
    config.tokens.clear();
    assert!(
        Runtime::new(config.clone(), Arc::clone(&fx.store)).is_err(),
        "no auth refuses to start"
    );
    config.insecure = true;
    let runtime = fx.runtime(config);
    runtime.refresh().await;
    let (status, _) = get(&runtime, "/metrics", &[]).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_session_runs_the_served_version() {
    let fx = Fixture::new();
    let v1 = fx.publish("Book tables.", "text").await;
    let runtime = fx.runtime(fx.config());
    runtime.refresh().await;
    let (addr, _stop, _task) = serve(&runtime).await;

    let mut ws = open(addr, TOKEN).await.unwrap();
    assert_eq!(start(&mut ws).await, v1);
    assert_eq!(fx.connects.load(Ordering::SeqCst), 1);
    fx.release_scripts();
    let mut said = String::new();
    while let Some(message) = next_json(&mut ws).await {
        if message["type"] == "textDelta" {
            said.push_str(message["text"].as_str().unwrap());
        }
        if message["type"] == "turnComplete" {
            break;
        }
    }
    assert_eq!(said, "hello");
    assert_eq!(runtime.active_sessions(), 1);
    drop(ws);
    eventually("the session to end", || runtime.active_sessions() == 0).await;
}

#[tokio::test]
async fn sessions_past_the_cap_get_503() {
    let fx = Fixture::new();
    fx.publish("Book tables.", "text").await;
    let mut config = fx.config();
    config.max_sessions = 1;
    let runtime = fx.runtime(config);
    runtime.refresh().await;
    let (addr, _stop, _task) = serve(&runtime).await;

    let first = open(addr, TOKEN).await.unwrap();
    assert_eq!(runtime.active_sessions(), 1);
    assert_eq!(
        http_status(open(addr, TOKEN).await),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let bearer = format!("Bearer {TOKEN}");
    let (_, body) = get(&runtime, "/metrics", &[("authorization", &bearer)]).await;
    assert!(
        body.contains("adk_runtime_sessions_refused_total{reason=\"full\"} 1"),
        "{body}"
    );

    // The slot frees when the session ends.
    drop(first);
    eventually("the slot to free", || runtime.active_sessions() == 0).await;
    let mut again = open(addr, TOKEN).await.unwrap();
    start(&mut again).await;
}

#[tokio::test]
async fn readyz_waits_for_every_bundle() {
    let fx = Fixture::new();
    let runtime = fx.runtime(fx.config());
    runtime.refresh().await;
    let (status, body) = get(&runtime, "/readyz", &[]).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("booking:prod"), "{body}");

    fx.publish("Book tables.", "text").await;
    runtime.refresh().await;
    assert_eq!(get(&runtime, "/readyz", &[]).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_spec_the_runtime_cannot_serve_is_not_loaded() {
    let fx = Fixture::new();
    let spec = SessionSpec::from_value(json!({
        "name": "booking",
        "instruction": "Remember the guest.",
        "memory": {},
        "flow": {"steps": [{"id": "serve", "terminal": true}]},
    }))
    .unwrap();
    let version = fx.store.push("booking", &spec, None).await.unwrap();
    fx.store
        .set_label("booking", "prod", &version.version)
        .await
        .unwrap();
    let runtime = fx.runtime(fx.config());
    let report = runtime.refresh().await;
    assert!(report.errors[0].contains("memory"), "{:?}", report.errors);
    assert!(runtime.readiness().is_err());
}

#[tokio::test]
async fn moving_a_label_changes_new_sessions_not_running_ones() {
    let fx = Fixture::new();
    let v1 = fx.publish("Book tables.", "text").await;
    let runtime = fx.runtime(fx.config());
    runtime.refresh().await;
    let (addr, _stop, _task) = serve(&runtime).await;

    let mut before = open(addr, TOKEN).await.unwrap();
    assert_eq!(start(&mut before).await, v1);

    let v2 = fx
        .publish("Book tables, and ask about seating.", "text")
        .await;
    assert_ne!(v1, v2);
    let report = runtime.refresh().await;
    assert_eq!(
        report.changed,
        [("booking:prod".to_string(), Some(v1.clone()), v2.clone())]
    );

    let mut after = open(addr, TOKEN).await.unwrap();
    assert_eq!(start(&mut after).await, v2);

    // The first session is still open, on the version it started with.
    assert_eq!(runtime.active_sessions(), 2);
    fx.release_scripts();
    assert!(next_json(&mut before).await.is_some());

    // Rolling back is moving the label again.
    fx.store.set_label("booking", "prod", &v1).await.unwrap();
    runtime.refresh().await;
    assert_eq!(runtime.bundle("booking").unwrap().version.version, v1);
}

#[tokio::test]
async fn the_refresh_loop_picks_up_a_moved_label() {
    let fx = Fixture::new();
    let v1 = fx.publish("Book tables.", "text").await;
    let mut config = fx.config();
    config.refresh_every = Duration::from_millis(20);
    let runtime = fx.runtime(config);
    runtime.refresh().await;
    let refresher = runtime.spawn_refresh_loop();

    let v2 = fx.publish("Book tables politely.", "text").await;
    eventually("the loop to load v2", || {
        runtime.bundle("booking").unwrap().version.version == v2
    })
    .await;

    // A requested refresh (SIGHUP) works too.
    fx.store.set_label("booking", "prod", &v1).await.unwrap();
    runtime.request_refresh();
    eventually("the rollback", || {
        runtime.bundle("booking").unwrap().version.version == v1
    })
    .await;
    refresher.abort();
}

#[tokio::test]
async fn draining_refuses_new_sessions_then_closes_the_rest() {
    let fx = Fixture::new();
    fx.publish("Book tables.", "text").await;
    let mut config = fx.config();
    config.drain_grace = Duration::from_millis(300);
    let runtime = fx.runtime(config);
    runtime.refresh().await;
    let (addr, stop, task) = serve(&runtime).await;

    let mut running = open(addr, TOKEN).await.unwrap();
    start(&mut running).await;

    stop.send(()).unwrap();
    eventually("drain to begin", || runtime.is_draining()).await;
    assert_eq!(
        http_status(open(addr, TOKEN).await),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        get(&runtime, "/readyz", &[]).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(get(&runtime, "/healthz", &[]).await.0, StatusCode::OK);

    // After the grace period the running session is told and closed, and
    // the server stops.
    let mut last_error = None;
    while let Some(message) = next_json(&mut running).await {
        if message["type"] == "error" {
            last_error = message["message"].as_str().map(str::to_string);
        }
    }
    assert_eq!(last_error.as_deref(), Some("server is shutting down"));
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("server stops after draining")
        .unwrap();
    assert_eq!(runtime.active_sessions(), 0);
}

#[tokio::test]
async fn drain_returns_early_when_sessions_finish() {
    let fx = Fixture::new();
    fx.publish("Book tables.", "text").await;
    let mut config = fx.config();
    config.drain_grace = Duration::from_secs(30);
    let runtime = fx.runtime(config);
    runtime.refresh().await;
    let (addr, _stop, _task) = serve(&runtime).await;
    let mut running = open(addr, TOKEN).await.unwrap();
    start(&mut running).await;

    let draining = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        async move { runtime.drain().await }
    });
    eventually("drain to begin", || runtime.is_draining()).await;
    running
        .send(tungstenite::Message::Text(
            json!({"type": "stop"}).to_string(),
        ))
        .await
        .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(5), draining)
        .await
        .expect("drain ends when the last session does")
        .unwrap();
    assert_eq!(closed, 0, "nothing had to be closed");
}

// ── Twilio ──────────────────────────────────────────────────────────────────

const TWILIO: &str = "twilio-auth-token";

fn twilio_config(fx: &Fixture) -> RuntimeConfig {
    let mut config = fx.config();
    config.twilio_auth_token = Some(TWILIO.into());
    config.public_url = Some("https://voice.example".into());
    config
}

async fn post_voice(
    runtime: &Arc<Runtime>,
    params: &[(&str, &str)],
    signature: Option<String>,
) -> (StatusCode, String) {
    let body: String = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params)
        .finish();
    let mut request = Request::builder()
        .method("POST")
        .uri("/twilio/voice/booking")
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(signature) = signature {
        request = request.header("x-twilio-signature", signature);
    }
    let response = runtime
        .router()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn sign(params: &[(&str, &str)]) -> String {
    let owned: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    twilio_signature(TWILIO, "https://voice.example/twilio/voice/booking", &owned)
}

/// The stream URL's path from a TwiML answer.
fn stream_path(twiml: &str) -> String {
    let start =
        twiml.find("wss://voice.example").expect("a stream URL") + "wss://voice.example".len();
    let end = twiml[start..].find('"').unwrap() + start;
    twiml[start..end].to_string()
}

#[tokio::test]
async fn the_twilio_webhook_needs_a_valid_signature() {
    let fx = Fixture::new();
    fx.publish("Answer the phone.", "audio").await;
    let runtime = fx.runtime(twilio_config(&fx));
    runtime.refresh().await;
    let params = [
        ("CallSid", "CA123"),
        ("From", "+15550100"),
        ("To", "+15550199"),
    ];

    assert_eq!(
        post_voice(&runtime, &params, None).await.0,
        StatusCode::FORBIDDEN
    );
    let tampered = [
        ("CallSid", "CA999"),
        ("From", "+15550100"),
        ("To", "+15550199"),
    ];
    assert_eq!(
        post_voice(&runtime, &tampered, Some(sign(&params))).await.0,
        StatusCode::FORBIDDEN
    );

    let (status, twiml) = post_voice(&runtime, &params, Some(sign(&params))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        twiml.contains("<Connect><Stream url=\"wss://voice.example/twilio/media/booking/"),
        "{twiml}"
    );

    // The media socket refuses a made-up token before upgrading.
    let (status, _) = get(&runtime, "/twilio/media/booking/1.2.3", &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn twilio_routes_are_off_without_twilio_credentials() {
    let fx = Fixture::new();
    fx.publish("Answer the phone.", "audio").await;
    let runtime = fx.runtime(fx.config());
    runtime.refresh().await;
    let params = [("CallSid", "CA123")];
    assert_eq!(
        post_voice(&runtime, &params, None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_full_instance_answers_twilio_with_busy() {
    let fx = Fixture::new();
    fx.publish("Answer the phone.", "audio").await;
    let runtime = fx.runtime(twilio_config(&fx));
    runtime.refresh().await;
    runtime.begin_drain();
    let params = [("CallSid", "CA123")];
    let (status, twiml) = post_voice(&runtime, &params, Some(sign(&params))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(twiml.contains("<Reject reason=\"busy\"/>"), "{twiml}");
}

async fn open_media(addr: SocketAddr, path: &str, call_sid: &str) -> Client {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}{path}"))
        .await
        .unwrap();
    let connected = json!({"event": "connected", "protocol": "Call", "version": "1.0.0"});
    let start = json!({
        "event": "start",
        "sequenceNumber": "1",
        "start": {
            "streamSid": "MZ1", "callSid": call_sid, "accountSid": "AC1",
            "tracks": ["inbound"],
            "mediaFormat": {"encoding": "audio/x-mulaw", "sampleRate": 8000, "channels": 1},
            "customParameters": {}
        },
        "streamSid": "MZ1"
    });
    for frame in [connected, start] {
        ws.send(tungstenite::Message::Text(frame.to_string()))
            .await
            .unwrap();
    }
    ws
}

#[tokio::test]
async fn a_stream_token_opens_one_session_for_its_own_call() {
    let fx = Fixture::new();
    fx.publish("Answer the phone.", "audio").await;
    let runtime = fx.runtime(twilio_config(&fx));
    runtime.refresh().await;
    let (addr, _stop, _task) = serve(&runtime).await;

    let params = [("CallSid", "CA123")];
    let (_, twiml) = post_voice(&runtime, &params, Some(sign(&params))).await;
    let path = stream_path(&twiml);

    // Another call's SID: the socket closes and no session starts.
    let mut wrong = open_media(addr, &path, "CA999").await;
    assert!(next_json(&mut wrong).await.is_none());
    assert_eq!(fx.connects.load(Ordering::SeqCst), 0);

    // The call the token was minted for gets a session.
    let _right = open_media(addr, &path, "CA123").await;
    eventually("the call session", || {
        fx.connects.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(runtime.active_sessions(), 1);

    // The token is spent.
    let mut replay = open_media(addr, &path, "CA123").await;
    assert!(next_json(&mut replay).await.is_none());
    assert_eq!(fx.connects.load(Ordering::SeqCst), 1);
}
