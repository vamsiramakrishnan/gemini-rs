//! The Studio's HTTP surface: the page itself (built from `apps/studio`
//! into `static/studio`) and the endpoints it calls. Validation, tests,
//! step-through, code generation and bundle storage all run on the real
//! spec machinery, so the Studio shows what a runtime would do.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, Query},
    http::StatusCode,
    response::{Html, IntoResponse, Json, Response},
    routing::{get, post, put},
};
use gemini_adk_fluent_rs::spec::{BundleRef, BundleStore, SessionSpec, open_store};
use serde::Deserialize;
use serde_json::json;

/// The Studio page and its API. `static_dir` is where `static/studio`
/// (the built front end) lives.
pub fn router(static_dir: &'static str) -> Router {
    let uri = std::env::var("ADK_BUNDLES").unwrap_or_else(|_| "bundles".to_string());
    let bundles = BundleState {
        store: open_store(&uri).map_err(|e| e.to_string()),
        uri,
    };
    let page = move || studio_page(static_dir);
    Router::new()
        .route("/studio", get(page))
        .route("/flows", get(page))
        .route("/api/flows/validate", post(validate_flow))
        .route("/api/flows/test", post(test_flow))
        .route("/api/flows/simulate", post(simulate_flow))
        .route("/api/flows/codegen", post(codegen_flow))
        .route("/api/flows/project", post(project_flow))
        .route("/api/flows/schema", get(flow_schema))
        .merge(
            Router::new()
                .route("/api/bundles", get(list_bundles))
                .route("/api/bundles/{name}", get(bundle_detail).post(push_bundle))
                .route("/api/bundles/{name}/labels/{label}", put(set_label))
                .route("/api/bundle", get(load_bundle))
                .with_state(bundles),
        )
}

/// The built Studio. Read at request time so a rebuild shows up on reload.
async fn studio_page(static_dir: &'static str) -> Response {
    match tokio::fs::read_to_string(format!("{static_dir}/studio/index.html")).await {
        Ok(html) => Html(html).into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            "The Studio is not built. Run `npm --prefix apps/studio ci && npm --prefix apps/studio run build`.",
        )
            .into_response(),
    }
}

/// Validate a session spec (or a bare flow) and return structured diagnostics.
///
/// Accepts the same JSON the Flow Studio edits and `flow-studio` sessions run:
/// a [`gemini_adk_fluent_rs::spec::SessionSpec`], or a bare `{"steps": …}`
/// flow document.
async fn validate_flow(Json(value): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let result = match gemini_adk_fluent_rs::spec::SessionSpec::from_value(value) {
        Ok(spec) => serde_json::to_value(spec.validate()).unwrap_or_default(),
        Err(message) => serde_json::json!({
            "valid": false,
            "errors": [message],
            "warnings": [],
            "mermaid": "",
            "tools": [],
            "steps": 0,
        }),
    };
    Json(result)
}

/// Run a spec's embedded conformance tests and conversation scenarios
/// offline — scripted conversations replayed through the real flow monitor
/// and conversation simulator, no model or API key involved.
async fn test_flow(Json(value): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let result = match gemini_adk_fluent_rs::spec::SessionSpec::from_value(value) {
        Ok(spec) => {
            let validation = spec.validate();
            if !validation.valid {
                serde_json::json!({
                    "valid": false,
                    "errors": validation.errors,
                    "reports": [],
                    "scenarios": [],
                })
            } else {
                serde_json::json!({
                    "valid": true,
                    "errors": [],
                    "reports": serde_json::to_value(spec.run_tests()).unwrap_or_default(),
                    "scenarios": serde_json::to_value(spec.run_scenarios().await)
                        .unwrap_or_default(),
                })
            }
        }
        Err(message) => serde_json::json!({
            "valid": false,
            "errors": [message],
            "reports": [],
            "scenarios": [],
        }),
    };
    Json(result)
}

/// The JSON Schema of the session spec document itself — for editor
/// autocomplete and for validating machine-authored specs.
async fn flow_schema() -> Json<serde_json::Value> {
    Json(gemini_adk_fluent_rs::spec::SessionSpec::json_schema())
}

/// Replay one embedded test event-by-event and return per-event flow
/// snapshots — the Studio's Preview scrubber. Body: `{"spec": …, "test": name}`.
async fn simulate_flow(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let test = body
        .get("test")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    let spec_value = body.get("spec").cloned().unwrap_or(serde_json::Value::Null);
    let result = match gemini_adk_fluent_rs::spec::SessionSpec::from_value(spec_value) {
        Ok(spec) => match gemini_adk_fluent_rs::spec::trace_test(&spec, &test) {
            Ok(snapshots) => serde_json::json!({
                "valid": true,
                "errors": [],
                "snapshots": serde_json::to_value(snapshots).unwrap_or_default(),
            }),
            Err(errors) => serde_json::json!({"valid": false, "errors": errors, "snapshots": []}),
        },
        Err(message) => serde_json::json!({"valid": false, "errors": [message], "snapshots": []}),
    };
    Json(result)
}

/// Generate the standalone Rust application a spec is equivalent to.
async fn codegen_flow(Json(value): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let result = match gemini_adk_fluent_rs::spec::SessionSpec::from_value(value) {
        Ok(spec) => serde_json::json!({
            "valid": true,
            "errors": [],
            "main_rs": spec.to_rust(),
            "cargo_toml": spec.to_cargo_toml(),
        }),
        Err(message) => serde_json::json!({"valid": false, "errors": [message]}),
    };
    Json(result)
}

/// Generate a project around a spec: `{"spec": …, "lang": "rust" | "python"
/// | "go"}`. Returns the files; nothing is written on the server.
async fn project_flow(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    use gemini_adk_fluent_rs::spec::{ProjectLanguage, SessionSpec};
    let language = body
        .get("lang")
        .and_then(|l| l.as_str())
        .unwrap_or("rust")
        .parse::<ProjectLanguage>();
    let spec = SessionSpec::from_value(body.get("spec").cloned().unwrap_or_default());
    let result = match (spec, language) {
        (Ok(spec), Ok(language)) => serde_json::json!({
            "valid": true,
            "errors": [],
            "files": spec.to_project(language),
        }),
        (Err(message), _) | (_, Err(message)) => {
            serde_json::json!({"valid": false, "errors": [message], "files": []})
        }
    };
    Json(result)
}

// ── Bundles ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct BundleState {
    /// The store `ADK_BUNDLES` names (default `./bundles`), or why it could
    /// not be opened.
    store: Result<Arc<dyn BundleStore>, String>,
    uri: String,
}

impl BundleState {
    fn store(&self) -> Result<&Arc<dyn BundleStore>, Box<Response>> {
        self.store
            .as_ref()
            .map_err(|e| Box::new(failure(StatusCode::SERVICE_UNAVAILABLE, e)))
    }
}

fn failure(status: StatusCode, message: impl std::fmt::Display) -> Response {
    (status, Json(json!({ "error": message.to_string() }))).into_response()
}

fn store_failure(error: gemini_adk_fluent_rs::spec::StoreError) -> Response {
    use gemini_adk_fluent_rs::spec::StoreError;
    let status = match &error {
        StoreError::NotFound(_) => StatusCode::NOT_FOUND,
        StoreError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
        StoreError::Backend(_) => StatusCode::BAD_GATEWAY,
    };
    failure(status, error)
}

/// Every bundle and its labels.
async fn list_bundles(axum::extract::State(state): axum::extract::State<BundleState>) -> Response {
    let store = match state.store() {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let names = match store.names().await {
        Ok(names) => names,
        Err(e) => return store_failure(e),
    };
    let mut bundles = Vec::new();
    for name in names {
        let labels = store.labels(&name).await.unwrap_or_default();
        bundles.push(json!({ "name": name, "labels": labels }));
    }
    Json(json!({ "store": state.uri, "bundles": bundles })).into_response()
}

/// One bundle's versions (oldest first) and labels.
async fn bundle_detail(
    axum::extract::State(state): axum::extract::State<BundleState>,
    Path(name): Path<String>,
) -> Response {
    let store = match state.store() {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let versions = match store.versions(&name).await {
        Ok(versions) => versions,
        Err(e) => return store_failure(e),
    };
    let labels = store.labels(&name).await.unwrap_or_default();
    Json(json!({ "versions": versions, "labels": labels })).into_response()
}

#[derive(Deserialize)]
struct PushBody {
    spec: serde_json::Value,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
}

/// Save a spec as a version, and optionally point labels at it.
async fn push_bundle(
    axum::extract::State(state): axum::extract::State<BundleState>,
    Path(name): Path<String>,
    Json(body): Json<PushBody>,
) -> Response {
    let store = match state.store() {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let spec = match SessionSpec::from_value(body.spec) {
        Ok(spec) => spec,
        Err(e) => return failure(StatusCode::UNPROCESSABLE_ENTITY, e),
    };
    let version = match store.push(&name, &spec, body.message.as_deref()).await {
        Ok(version) => version,
        Err(e) => return store_failure(e),
    };
    for label in &body.labels {
        if let Err(e) = store.set_label(&name, label, &version.version).await {
            return store_failure(e);
        }
    }
    Json(json!({ "version": version })).into_response()
}

#[derive(Deserialize)]
struct LabelBody {
    version: String,
}

/// Point a label at a version: promote or roll back.
async fn set_label(
    axum::extract::State(state): axum::extract::State<BundleState>,
    Path((name, label)): Path<(String, String)>,
    Json(body): Json<LabelBody>,
) -> Response {
    let store = match state.store() {
        Ok(store) => store,
        Err(response) => return *response,
    };
    match store.set_label(&name, &label, &body.version).await {
        Ok(version) => Json(json!({ "version": version })).into_response(),
        Err(e) => store_failure(e),
    }
}

#[derive(Deserialize)]
struct LoadQuery {
    #[serde(rename = "ref")]
    reference: String,
}

/// Load a version by reference: `name`, `name@version` or `name:label`.
async fn load_bundle(
    axum::extract::State(state): axum::extract::State<BundleState>,
    Query(query): Query<LoadQuery>,
) -> Response {
    let store = match state.store() {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let (name, reference) = BundleRef::parse(&query.reference);
    match store.get(&name, &reference).await {
        Ok((version, spec)) => Json(json!({ "version": version, "spec": spec })).into_response(),
        Err(e) => store_failure(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn spec() -> serde_json::Value {
        json!({ "name": "booking", "flow": { "steps": [{ "id": "s", "terminal": true }] } })
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[test]
    fn the_studio_tests_run_against_the_current_schema() {
        let snapshot: serde_json::Value = serde_json::from_str(include_str!(
            "../../studio/src/test/session-spec.schema.json"
        ))
        .unwrap();
        assert!(
            snapshot == SessionSpec::json_schema(),
            "apps/studio/src/test/session-spec.schema.json is stale: refresh it with \
             `cargo run -p gemini-adk-cli-rs -- spec schema > apps/studio/src/test/session-spec.schema.json`"
        );
    }

    #[tokio::test]
    async fn the_studio_saves_lists_labels_and_loads_bundles() {
        let dir = std::env::temp_dir().join(format!("studio-bundles-{}", std::process::id()));
        let store: Arc<dyn BundleStore> = open_store(dir.to_str().unwrap()).unwrap();
        let app = Router::new()
            .route("/api/bundles", get(list_bundles))
            .route("/api/bundles/{name}", get(bundle_detail).post(push_bundle))
            .route("/api/bundles/{name}/labels/{label}", put(set_label))
            .route("/api/bundle", get(load_bundle))
            .with_state(BundleState {
                store: Ok(store),
                uri: "test".into(),
            });

        let (status, pushed) = call(
            &app,
            "POST",
            "/api/bundles/booking",
            Some(json!({ "spec": spec(), "message": "first", "labels": ["staging"] })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{pushed}");
        let version = pushed["version"]["version"].as_str().unwrap().to_string();

        let (_, list) = call(&app, "GET", "/api/bundles", None).await;
        assert_eq!(list["bundles"][0]["name"], "booking");
        assert_eq!(list["bundles"][0]["labels"]["staging"], version);

        let (status, _) = call(
            &app,
            "PUT",
            "/api/bundles/booking/labels/prod",
            Some(json!({ "version": &version[..6] })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, loaded) = call(&app, "GET", "/api/bundle?ref=booking:prod", None).await;
        assert_eq!(loaded["version"]["version"], version);
        assert_eq!(loaded["spec"]["name"], "booking");

        // An invalid spec is refused with the validation errors.
        let (status, refused) = call(&app, "POST", "/api/bundles/booking", Some(json!({ "spec": { "name": "x", "flow": { "steps": [{ "id": "s", "allow": ["nope"] }] } } }))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        let (status, _) = call(&app, "GET", "/api/bundle?ref=booking:nope", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(dir);
    }
}
