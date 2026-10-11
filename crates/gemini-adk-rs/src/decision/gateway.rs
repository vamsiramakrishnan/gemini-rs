//! Vercel AI Gateway's decision API (`POST /v1/evaluate`).

use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use super::{
    Answer, DecisionModel, DecisionRequest, DecisionResponse, DecisionUsage, FallbackWhen,
};
use crate::llm::LlmError;

/// A decision model served by Vercel AI Gateway, such as TypeSafe AI's Jev.
///
/// Authenticates with `AI_GATEWAY_API_KEY`, or with a `VERCEL_OIDC_TOKEN`
/// (what `vercel env pull` writes and what deployments on Vercel receive).
///
/// ```no_run
/// use gemini_adk_rs::decision::{DecisionModel, DecisionRequest, GatewayDecisionModel, Question};
/// use serde_json::json;
///
/// # async fn run() -> Result<(), gemini_adk_rs::llm::LlmError> {
/// let jev = GatewayDecisionModel::from_env()?;
/// let response = jev
///     .decide(DecisionRequest::new(
///         json!({ "caller": "Yes, please book it." }),
///         [("confirmed", Question::boolean("Did the caller agree to book?"))],
///     ))
///     .await?;
/// println!("{:?} in {:?}", response.answers["confirmed"], response.latency);
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct GatewayDecisionModel {
    client: reqwest::Client,
    base_url: String,
    model: String,
    token: String,
    timeout: Duration,
    options: Value,
}

impl std::fmt::Debug for GatewayDecisionModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the token.
        f.debug_struct("GatewayDecisionModel")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl GatewayDecisionModel {
    /// AI Gateway's address.
    pub const DEFAULT_BASE_URL: &'static str = "https://ai-gateway.vercel.sh";
    /// TypeSafe AI's Jev, the default model.
    pub const JEV: &'static str = "typesafe-ai/jev";
    /// How long a decision may take before the call fails.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

    /// A client for `model`, authenticating with `token`.
    pub fn new(model: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: Self::DEFAULT_BASE_URL.to_string(),
            model: model.into(),
            token: token.into(),
            timeout: Self::DEFAULT_TIMEOUT,
            options: Value::Null,
        }
    }

    /// A client configured from the environment:
    ///
    /// | Variable | Use |
    /// |---|---|
    /// | `AI_GATEWAY_API_KEY`, else `VERCEL_OIDC_TOKEN` | Credential (required) |
    /// | `AI_GATEWAY_DECISION_MODEL` | Model, default `typesafe-ai/jev` |
    /// | `AI_GATEWAY_BASE_URL` | Gateway address, default `https://ai-gateway.vercel.sh` |
    pub fn from_env() -> Result<Self, LlmError> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let token = var("AI_GATEWAY_API_KEY")
            .or_else(|| var("VERCEL_OIDC_TOKEN"))
            .ok_or_else(|| {
                LlmError::Auth(
                    "no AI Gateway credential: set AI_GATEWAY_API_KEY (for example in \
                     .env.local), or run `vercel env pull` for a VERCEL_OIDC_TOKEN"
                        .into(),
                )
            })?;
        let model = var("AI_GATEWAY_DECISION_MODEL").unwrap_or_else(|| Self::JEV.to_string());
        let mut this = Self::new(model, token);
        if let Some(base) = var("AI_GATEWAY_BASE_URL") {
            this.base_url = base.trim_end_matches('/').to_string();
        }
        Ok(this)
    }

    /// Use another Gateway address (a proxy, or a local server in tests).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Fail a decision that takes longer than `timeout`.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Ask the Gateway to route only to providers with zero data retention.
    #[must_use]
    pub fn zero_data_retention(mut self, on: bool) -> Self {
        merge_json(
            &mut self.options,
            &object("gateway", json!({ "zeroDataRetention": on })),
        );
        self
    }

    /// Restrict which providers may serve the model.
    #[must_use]
    pub fn only_providers<S: Into<String>>(
        mut self,
        providers: impl IntoIterator<Item = S>,
    ) -> Self {
        let only: Vec<String> = providers.into_iter().map(Into::into).collect();
        merge_json(
            &mut self.options,
            &object("gateway", json!({ "only": only })),
        );
        self
    }

    /// Rerun the decision with `model` when `when` matches the primary
    /// answers (a Gateway decision fallback). `model` may be a language
    /// model, which answers through structured output and reports no
    /// confidence. Both stages are billed and their latencies add.
    #[must_use]
    pub fn fallback(mut self, model: impl Into<String>, when: FallbackWhen) -> Self {
        let entry = json!({ "model": model.into(), "when": when.to_value() });
        merge_json(
            &mut self.options,
            &object("gateway", json!({ "models": [entry] })),
        );
        self
    }

    /// The request body for `request`.
    pub fn body(&self, request: &DecisionRequest) -> Value {
        let mut body = json!({
            "model": self.model,
            "state": request.state,
            "questions": request.questions,
        });
        let mut options = self.options.clone();
        if let Some(extra) = &request.provider_options {
            merge_json(&mut options, extra);
        }
        if !options.is_null() {
            body["providerOptions"] = options;
        }
        body
    }

    /// Read a `/v1/evaluate` response.
    pub fn parse(status: u16, body: &str, latency: Duration) -> Result<DecisionResponse, LlmError> {
        let value: Value = serde_json::from_str(body).map_err(|e| LlmError::Api {
            status,
            message: format!("unreadable response ({e}): {}", truncate(body, 300)),
        })?;
        if !(200..300).contains(&status) {
            let message = value["error"]["message"]
                .as_str()
                .map_or_else(|| truncate(body, 300), str::to_string);
            return Err(match status {
                401 => LlmError::Auth(format!(
                    "AI Gateway rejected the credential: {message}. Check AI_GATEWAY_API_KEY"
                )),
                _ => LlmError::Api { status, message },
            });
        }
        let answers = value["answers"]
            .as_object()
            .ok_or_else(|| LlmError::Api {
                status,
                message: format!("response has no answers: {}", truncate(body, 300)),
            })?
            .iter()
            .map(|(k, v)| (k.clone(), Answer::from_value(v.clone())))
            .collect();
        let usage = value.get("usage").map(|u| DecisionUsage {
            input_tokens: u["inputTokens"].as_u64().unwrap_or(0),
            output_tokens: u["outputTokens"].as_u64().unwrap_or(0),
        });
        let gateway = &value["providerMetadata"]["gateway"];
        let fallback_triggered_by = gateway["routing"]["modelAttempts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|attempt| attempt["triggeredBy"].as_array())
            .flatten()
            .map(|t| {
                (
                    t["question"].as_str().unwrap_or_default().to_string(),
                    t["reason"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        Ok(DecisionResponse {
            model: value["model"].as_str().unwrap_or_default().to_string(),
            answers,
            usage,
            cost: gateway["cost"].as_str().map(str::to_string),
            fallback_triggered_by,
            latency,
            provider_metadata: value["providerMetadata"].clone(),
        })
    }
}

/// Merge `overlay` into `base`, objects recursively; other values replace.
fn merge_json(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                merge_json(b.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (b, o) => *b = o.clone(),
    }
}

/// A JSON object with one key, for building nested options.
fn object(key: &str, value: Value) -> Value {
    let mut m = Map::new();
    m.insert(key.to_string(), value);
    Value::Object(m)
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[async_trait]
impl DecisionModel for GatewayDecisionModel {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn decide(&self, request: DecisionRequest) -> Result<DecisionResponse, LlmError> {
        for (id, question) in &request.questions {
            question
                .validate()
                .map_err(|e| LlmError::Config(format!("question '{id}': {e}")))?;
        }
        let started = Instant::now();
        let response = self
            .client
            .post(format!("{}/v1/evaluate", self.base_url))
            .bearer_auth(&self.token)
            .timeout(self.timeout)
            .json(&self.body(&request))
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        let text = response
            .text()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        let latency = started.elapsed();
        let parsed = Self::parse(status, &text, latency);
        match &parsed {
            Ok(r) => tracing::info!(
                model = %r.model,
                ms = latency.as_millis() as u64,
                questions = r.answers.len(),
                fallback = !r.fallback_triggered_by.is_empty(),
                "decision"
            ),
            Err(e) => {
                tracing::warn!(model = %self.model, ms = latency.as_millis() as u64, "decision failed: {e}");
            }
        }
        parsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::Question;

    fn jev() -> GatewayDecisionModel {
        GatewayDecisionModel::new(GatewayDecisionModel::JEV, "test-token")
    }

    #[test]
    fn the_body_carries_state_questions_and_options() {
        let model = jev().zero_data_retention(true).fallback(
            "google/gemini-3.8-flash",
            FallbackWhen::ProbabilityBetween {
                question: Some("confirmed".into()),
                low: 0.3,
                high: 0.8,
            },
        );
        let mut request = DecisionRequest::new(
            json!({ "caller": "yes" }),
            [("confirmed", Question::boolean("Agreed?"))],
        );
        request.provider_options = Some(json!({ "gateway": { "only": ["typesafe-ai"] } }));
        assert_eq!(
            model.body(&request),
            json!({
                "model": "typesafe-ai/jev",
                "state": { "caller": "yes" },
                "questions": { "confirmed": { "type": "boolean", "instructions": "Agreed?" } },
                "providerOptions": { "gateway": {
                    "zeroDataRetention": true,
                    "only": ["typesafe-ai"],
                    "models": [ { "model": "google/gemini-3.8-flash",
                                  "when": { "question": "confirmed", "probabilityBetween": [0.3, 0.8] } } ]
                } }
            })
        );
        assert!(
            jev().body(&request).get("providerOptions").is_some(),
            "per-request options alone"
        );
        let plain = DecisionRequest::new(json!("x"), [("a", Question::boolean("A?"))]);
        assert!(jev().body(&plain).get("providerOptions").is_none());
    }

    #[test]
    fn a_documented_response_is_read() {
        let body = r#"{
          "model": "typesafe-ai/jev",
          "answers": { "refund": { "type": "boolean", "probability": 0.98 } },
          "usage": { "inputTokens": 275, "outputTokens": 20 },
          "providerMetadata": { "gateway": {
            "routing": { "originalModelId": "typesafe-ai/jev", "finalProvider": "typesafe-ai" },
            "cost": "0.00001155", "generationId": "gen_1" } }
        }"#;
        let r = GatewayDecisionModel::parse(200, body, Duration::from_millis(120)).unwrap();
        assert_eq!(r.model, "typesafe-ai/jev");
        assert_eq!(r.answers["refund"].probability(), Some(0.98));
        assert_eq!(
            r.usage,
            Some(DecisionUsage {
                input_tokens: 275,
                output_tokens: 20
            })
        );
        assert_eq!(r.cost.as_deref(), Some("0.00001155"));
        assert!(r.fallback_triggered_by.is_empty());
    }

    #[test]
    fn a_triggered_fallback_is_reported() {
        let body = r#"{
          "model": "openai/gpt-6-astra",
          "answers": { "intent": { "type": "choice", "choice": "billing" } },
          "providerMetadata": { "gateway": { "routing": { "modelAttempts": [
            { "canonicalSlug": "typesafe-ai/jev", "success": true },
            { "canonicalSlug": "openai/gpt-6-astra", "success": true,
              "triggeredBy": [ { "question": "intent", "reason": "confidence_below" } ] }
          ] } } }
        }"#;
        let r = GatewayDecisionModel::parse(200, body, Duration::ZERO).unwrap();
        assert_eq!(r.model, "openai/gpt-6-astra");
        assert_eq!(
            r.fallback_triggered_by,
            vec![("intent".to_string(), "confidence_below".to_string())]
        );
        assert_eq!(
            r.answers["intent"].certainty(),
            None,
            "a language model reports none"
        );
    }

    #[test]
    fn errors_keep_the_gateway_message() {
        let restricted = r#"{"error":{"message":"Free tier users do not have access to this model.","type":"no_providers_available"}}"#;
        match GatewayDecisionModel::parse(403, restricted, Duration::ZERO) {
            Err(LlmError::Api {
                status: 403,
                message,
            }) => {
                assert!(message.contains("Free tier"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        let auth = r#"{"error":{"message":"Authentication failed","type":"authentication_error"}}"#;
        assert!(matches!(
            GatewayDecisionModel::parse(401, auth, Duration::ZERO),
            Err(LlmError::Auth(_))
        ));
        let busy = GatewayDecisionModel::parse(503, "upstream busy", Duration::ZERO).unwrap_err();
        assert!(busy.is_retryable(), "{busy:?}");
    }

    #[test]
    fn provider_options_merge_recursively() {
        let mut base = json!({ "gateway": { "zeroDataRetention": true, "only": ["typesafe-ai"] } });
        merge_json(
            &mut base,
            &json!({ "gateway": { "only": ["digitalocean"] }, "openai": { "x": 1 } }),
        );
        assert_eq!(
            base,
            json!({ "gateway": { "zeroDataRetention": true, "only": ["digitalocean"] }, "openai": { "x": 1 } })
        );
    }

    #[test]
    fn the_token_is_never_printed() {
        let printed = format!("{:?}", jev());
        assert!(!printed.contains("test-token"), "{printed}");
    }

    #[tokio::test]
    async fn an_invalid_question_fails_before_the_call() {
        let none: [(&str, &str); 0] = [];
        let err = jev()
            .with_base_url("http://127.0.0.1:9")
            .decide(DecisionRequest::new(
                json!("x"),
                [("pick", Question::choice("Pick.", none))],
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Config(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_call_reaches_the_gateway_and_reads_the_answers() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 4096];
            // Read until the JSON body has arrived.
            while !String::from_utf8_lossy(&seen).contains("\"questions\"") {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&buf[..n]);
            }
            let body = r#"{"model":"typesafe-ai/jev","answers":{"confirmed":{"type":"boolean","probability":0.93}}}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&seen).to_string()
        });
        let model = jev().with_base_url(format!("http://{addr}"));
        let response = model
            .decide(DecisionRequest::new(
                json!("Caller: yes"),
                [("confirmed", Question::boolean("Agreed?"))],
            ))
            .await
            .unwrap();
        assert_eq!(response.answers["confirmed"].probability(), Some(0.93));
        let request = server.await.unwrap();
        assert!(request.starts_with("POST /v1/evaluate"), "{request}");
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer test-token"),
            "{request}"
        );
    }
}
