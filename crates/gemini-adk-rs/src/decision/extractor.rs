//! [`DecisionExtractor`]: a decision model as a turn extractor.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use super::{Answer, DecisionModel, DecisionRequest, Question};
use crate::live::extractor::{
    ExtractionTrigger, FieldPromotion, LlmExtractor, MergePolicy, TurnExtractor,
    nothing_new_from_caller,
};
use crate::live::transcript::TranscriptTurn;
use crate::llm::{BaseLlm, LlmError};
use crate::state::State;

/// The probability a boolean answer needs to count as true, when a
/// [`Promote`] does not say.
pub const DEFAULT_BOOLEAN_THRESHOLD: f64 = 0.85;
/// The certainty a choice or score answer needs, when a [`Promote`] does
/// not say.
pub const DEFAULT_CERTAINTY_THRESHOLD: f64 = 0.6;
/// How long a [`DecisionExtractor`] waits for the decision model before it
/// treats the call as failed. Jev's p90 through AI Gateway was under 0.6 s
/// even ten calls at a time, with rare multi-second outliers.
pub const DEFAULT_DECISION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// The option a choice question gets from [`DecisionQuestion::none`].
pub const NONE_OPTION: &str = "none_of_these";

/// How one question's answer becomes a state key.
#[derive(Debug, Clone, PartialEq)]
pub struct Promote {
    /// The state key written.
    pub to: String,
    /// For a boolean, the `P(true)` needed to write `true`; for a choice or
    /// score, the certainty needed (see [`Answer::certainty`]). `None` takes
    /// [`DEFAULT_BOOLEAN_THRESHOLD`] or [`DEFAULT_CERTAINTY_THRESHOLD`].
    pub at_least: Option<f64>,
    /// For a boolean, also write `false` when `P(true)` is at most
    /// `1 - at_least`. Off by default: a signal such as a confirmation
    /// latches true and is never written false by a later turn.
    pub write_false: bool,
    /// Whether a value already in state is kept or replaced.
    pub merge: MergePolicy,
}

impl Promote {
    /// Write the answer to `key`, replacing any value there.
    pub fn to(key: impl Into<String>) -> Self {
        Self {
            to: key.into(),
            at_least: None,
            write_false: false,
            merge: MergePolicy::Overwrite,
        }
    }

    /// Require this probability (boolean) or certainty (choice, score).
    #[must_use]
    pub fn at_least(mut self, threshold: f64) -> Self {
        self.at_least = Some(threshold);
        self
    }

    /// Also write `false` for a confident no.
    #[must_use]
    pub fn write_false(mut self) -> Self {
        self.write_false = true;
        self
    }

    /// Keep a value already in state.
    #[must_use]
    pub fn keep_known(mut self) -> Self {
        self.merge = MergePolicy::KeepKnown;
        self
    }
}

/// One question a [`DecisionExtractor`] asks, and where its answer goes.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionQuestion {
    /// The answer's field name, and the question's key in the request.
    pub id: String,
    /// The question.
    pub question: Question,
    /// For a choice: a state path (`key` or `key.field.0`) holding the
    /// options, read at each turn. Each element of the array there is an
    /// option: a string, a number, or an object (keyed by its `id`, `value`
    /// or `name` field, else by its JSON text). Static `criteria` are used
    /// when the path holds nothing.
    pub options_from: Option<String>,
    /// Ask only while one of these stages is active (`flow:active`). Empty
    /// asks on every turn.
    pub active_in: Vec<String>,
    /// For a choice: add the option [`NONE_OPTION`] with this description.
    /// Choosing it decides nothing. A choice always picks some option, so a
    /// question such as "which offered time did the caller pick?" needs a
    /// way to say the caller has not picked yet.
    pub none: Option<String>,
    /// Where the answer goes. `None` keeps it in the extractor's result only.
    pub promote: Option<Promote>,
}

impl DecisionQuestion {
    /// A question with no promotion.
    pub fn new(id: impl Into<String>, question: Question) -> Self {
        Self {
            id: id.into(),
            question,
            options_from: None,
            active_in: Vec::new(),
            none: None,
            promote: None,
        }
    }

    /// Add a "none of these" option meaning `description`; see
    /// [`none`](Self::none).
    #[must_use]
    pub fn or_none(mut self, description: impl Into<String>) -> Self {
        self.none = Some(description.into());
        self
    }

    /// Read a choice's options from a state path.
    #[must_use]
    pub fn options_from(mut self, path: impl Into<String>) -> Self {
        self.options_from = Some(path.into());
        self
    }

    /// Ask only while one of `stages` is active.
    #[must_use]
    pub fn active_in<S: Into<String>>(mut self, stages: impl IntoIterator<Item = S>) -> Self {
        self.active_in = stages.into_iter().map(Into::into).collect();
        self
    }

    /// Promote the answer.
    #[must_use]
    pub fn promote(mut self, promote: Promote) -> Self {
        self.promote = Some(promote);
        self
    }

    fn threshold(&self) -> f64 {
        let default = match self.question {
            Question::Boolean { .. } => DEFAULT_BOOLEAN_THRESHOLD,
            _ => DEFAULT_CERTAINTY_THRESHOLD,
        };
        self.promote
            .as_ref()
            .and_then(|p| p.at_least)
            .unwrap_or(default)
    }
}

/// A question as asked this turn: options resolved, and the option keys
/// mapped back to the values they stand for.
struct Asked<'a> {
    spec: &'a DecisionQuestion,
    question: Question,
    values: BTreeMap<String, Value>,
}

/// What became of one answer.
enum Outcome {
    /// Decided: the value to promote (`Null` for a confident no that is not
    /// written).
    Decided(Value),
    /// Inside the uncertain band, refused, or unreadable.
    Uncertain,
}

/// A [`TurnExtractor`] that asks a decision model typed questions about the
/// latest turns and promotes the confident answers into state.
///
/// Each turn it sends the decision model one state:
///
/// ```json
/// {
///   "conversation": [{ "caller": "…" }, { "tool": "check_availability", "result": "…" }, { "agent": "…" }],
///   "facts": { "party_size": 4 },
///   "active_stages": ["confirm"]
/// }
/// ```
///
/// and one question per [`DecisionQuestion`] that applies to the active
/// stages. The extractor's result, stored under its name, holds each
/// decided value by question id (null when undecided) and a `_decision`
/// object with the raw answers, the model, the latency and which answers
/// were uncertain.
///
/// An answer inside the uncertain band is not promoted. With
/// [`with_llm_fallback`](Self::with_llm_fallback), the uncertain questions
/// are asked again of a language model, through structured output, and its
/// answers are promoted instead.
///
/// The promotions are ordinary [`FieldPromotion`]s, so a commit guard that
/// reads a promoted key gets the same refresh as one fed by an
/// [`LlmExtractor`]: a commit refused only on that key runs this extractor
/// over the turn in progress before the refusal stands.
pub struct DecisionExtractor {
    name: String,
    model: Arc<dyn DecisionModel>,
    window_size: usize,
    questions: Vec<DecisionQuestion>,
    facts: Vec<String>,
    rules: Vec<FieldPromotion>,
    trigger: ExtractionTrigger,
    fallback: Option<Arc<dyn BaseLlm>>,
    provider_options: Option<Value>,
    timeout: std::time::Duration,
}

impl DecisionExtractor {
    /// An extractor named `name` (its state key), reading `window_size`
    /// turns.
    pub fn new(name: impl Into<String>, model: Arc<dyn DecisionModel>, window_size: usize) -> Self {
        Self {
            name: name.into(),
            model,
            window_size,
            questions: Vec::new(),
            facts: Vec::new(),
            rules: Vec::new(),
            trigger: ExtractionTrigger::EveryTurn,
            fallback: None,
            provider_options: None,
            timeout: DEFAULT_DECISION_TIMEOUT,
        }
    }

    /// Give up on the decision model after `timeout` (default
    /// [`DEFAULT_DECISION_TIMEOUT`]). The turn pipeline waits for every
    /// extraction, so a slow decision delays the next tool call. A timed-out
    /// or failed call goes to the language-model fallback when there is one.
    #[must_use]
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Ask `question` each turn.
    #[must_use]
    pub fn question(mut self, question: DecisionQuestion) -> Self {
        if let Some(p) = &question.promote {
            self.rules.push(FieldPromotion {
                field: question.id.clone(),
                state_key: p.to.clone(),
                merge: p.merge,
                accept: None,
            });
        }
        self.questions.push(question);
        self
    }

    /// Include these state keys in the decision state, under `facts`.
    #[must_use]
    pub fn facts<S: Into<String>>(mut self, keys: impl IntoIterator<Item = S>) -> Self {
        self.facts = keys.into_iter().map(Into::into).collect();
        self
    }

    /// Ask `llm` the questions whose answers were uncertain.
    #[must_use]
    pub fn with_llm_fallback(mut self, llm: Arc<dyn BaseLlm>) -> Self {
        self.fallback = Some(llm);
        self
    }

    /// When the extractor runs.
    #[must_use]
    pub fn with_trigger(mut self, trigger: ExtractionTrigger) -> Self {
        self.trigger = trigger;
        self
    }

    /// Provider options sent with every request, such as a Gateway
    /// decision fallback.
    #[must_use]
    pub fn with_provider_options(mut self, options: Value) -> Self {
        self.provider_options = Some(options);
        self
    }

    /// The questions asked, in order.
    pub fn questions(&self) -> &[DecisionQuestion] {
        &self.questions
    }

    fn asked<'a>(&'a self, state: &State) -> Vec<Asked<'a>> {
        let active: Vec<String> = state.get("flow:active").unwrap_or_default();
        self.questions
            .iter()
            .filter(|q| q.active_in.is_empty() || q.active_in.iter().any(|s| active.contains(s)))
            .filter_map(|spec| {
                let mut question = spec.question.clone();
                let mut values = BTreeMap::new();
                if let (Some(path), Question::Choice { criteria, .. }) =
                    (&spec.options_from, &mut question)
                {
                    let options = read_path(state, path)
                        .and_then(|v| v.as_array().cloned())
                        .unwrap_or_default();
                    if !options.is_empty() {
                        criteria.clear();
                        for option in options.into_iter().take(super::MAX_CHOICE_OPTIONS) {
                            let key = option_key(&option);
                            let description = match &option {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            criteria.insert(key.clone(), description);
                            values.insert(key, option);
                        }
                    }
                }
                if let (Some(none), Question::Choice { criteria, .. }) = (&spec.none, &mut question)
                {
                    criteria.insert(NONE_OPTION.to_string(), none.clone());
                    values.insert(NONE_OPTION.to_string(), Value::Null);
                }
                question.validate().ok().map(|()| Asked {
                    spec,
                    question,
                    values,
                })
            })
            .collect()
    }

    fn decision_state(&self, window: &[TranscriptTurn], state: &State) -> Value {
        let mut conversation = Vec::new();
        for turn in window {
            if !turn.user.trim().is_empty() {
                conversation.push(json!({ "caller": turn.user.trim() }));
            }
            for call in &turn.tool_calls {
                conversation.push(json!({ "tool": call.name, "result": call.result_summary }));
            }
            if !turn.model.trim().is_empty() {
                conversation.push(json!({ "agent": turn.model.trim() }));
            }
        }
        let facts: Map<String, Value> = self
            .facts
            .iter()
            .filter_map(|k| {
                state
                    .get_raw(k)
                    .filter(|v| !v.is_null())
                    .map(|v| (k.clone(), v))
            })
            .collect();
        let mut out = json!({ "conversation": conversation });
        if !facts.is_empty() {
            out["facts"] = Value::Object(facts);
        }
        let active: Vec<String> = state.get("flow:active").unwrap_or_default();
        if !active.is_empty() {
            out["active_stages"] = json!(active);
        }
        out
    }

    fn outcome(asked: &Asked<'_>, answer: Option<&Answer>) -> Outcome {
        let threshold = asked.spec.threshold();
        let write_false = asked.spec.promote.as_ref().is_some_and(|p| p.write_false);
        match answer {
            Some(Answer::Boolean { probability }) => {
                if *probability >= threshold {
                    Outcome::Decided(json!(true))
                } else if *probability <= 1.0 - threshold {
                    Outcome::Decided(if write_false {
                        json!(false)
                    } else {
                        Value::Null
                    })
                } else {
                    Outcome::Uncertain
                }
            }
            Some(answer @ Answer::Choice { choice, .. }) => {
                if answer.certainty().unwrap_or(0.0) >= threshold {
                    let value = asked
                        .values
                        .get(choice)
                        .cloned()
                        .unwrap_or_else(|| json!(choice));
                    Outcome::Decided(value)
                } else {
                    Outcome::Uncertain
                }
            }
            Some(answer @ Answer::Score { score, .. }) => {
                if answer.certainty().unwrap_or(0.0) >= threshold {
                    Outcome::Decided(json!(score))
                } else {
                    Outcome::Uncertain
                }
            }
            Some(Answer::Refusal(_) | Answer::Other(_)) | None => Outcome::Uncertain,
        }
    }

    /// Ask `llm` the `uncertain` questions through structured output.
    async fn ask_fallback(
        &self,
        llm: &Arc<dyn BaseLlm>,
        window: &[TranscriptTurn],
        uncertain: &[&Asked<'_>],
    ) -> Result<Map<String, Value>, LlmError> {
        let mut properties = Map::new();
        for asked in uncertain {
            let id = &asked.spec.id;
            let schema = match &asked.question {
                Question::Boolean {
                    instructions,
                    criteria,
                } => {
                    let mut d = instructions.clone();
                    if let Some(c) = criteria {
                        d.push_str(&format!(
                            " True when {}; false when {}.",
                            c.when_true, c.when_false
                        ));
                    }
                    json!({ "type": "boolean", "description": d })
                }
                Question::Choice {
                    instructions,
                    criteria,
                } => {
                    let options: Vec<String> = criteria
                        .iter()
                        .map(|(k, d)| {
                            if k == d {
                                k.clone()
                            } else {
                                format!("{k}: {d}")
                            }
                        })
                        .collect();
                    json!({
                        "type": "string",
                        "enum": criteria.keys().collect::<Vec<_>>(),
                        "description": format!("{instructions} Options: {}.", options.join("; ")),
                    })
                }
                Question::Score {
                    instructions,
                    criteria,
                } => {
                    let levels: Vec<String> = criteria
                        .iter()
                        .enumerate()
                        .map(|(i, d)| format!("{i} = {d}"))
                        .collect();
                    json!({
                        "type": "integer",
                        "minimum": 0,
                        "maximum": criteria.len() - 1,
                        "description": format!("{instructions} Levels: {}.", levels.join("; ")),
                    })
                }
            };
            properties.insert(id.clone(), schema);
        }
        let fallback = LlmExtractor::new(
            format!("{}:fallback", self.name),
            llm.clone(),
            "Answer each field's question about the conversation, judging the caller's latest \
             words in the context of the turns before them.",
            self.window_size,
        )
        .with_schema(json!({ "type": "object", "properties": properties }));
        let value = fallback.extract(window).await?;
        Ok(value.as_object().cloned().unwrap_or_default())
    }
}

/// An option's key: its own text for a string or number; for an object, its
/// `id`, `value` or `name` field, else its JSON text.
fn option_key(option: &Value) -> String {
    match option {
        Value::String(s) => s.clone(),
        Value::Object(m) => ["id", "value", "name"]
            .iter()
            .find_map(|k| m.get(*k))
            .map_or_else(
                || option.to_string(),
                |v| v.as_str().map_or_else(|| v.to_string(), str::to_string),
            ),
        other => other.to_string(),
    }
}

/// Read `key` or `key.field.0` from state.
fn read_path(state: &State, path: &str) -> Option<Value> {
    let mut parts = path.split('.');
    let mut value = state.get_raw(parts.next()?)?;
    for part in parts {
        value = match value {
            Value::Object(mut m) => m.remove(part)?,
            Value::Array(mut a) => {
                let i: usize = part.parse().ok()?;
                if i < a.len() {
                    a.swap_remove(i)
                } else {
                    return None;
                }
            }
            _ => return None,
        };
    }
    Some(value)
}

#[async_trait]
impl TurnExtractor for DecisionExtractor {
    fn name(&self) -> &str {
        &self.name
    }

    fn window_size(&self) -> usize {
        self.window_size
    }

    fn should_extract(&self, window: &[TranscriptTurn]) -> bool {
        // As for an LlmExtractor: a turn the caller said nothing in holds
        // nothing new to decide.
        !(self.trigger == ExtractionTrigger::EveryTurn && nothing_new_from_caller(window))
    }

    fn trigger(&self) -> ExtractionTrigger {
        self.trigger.clone()
    }

    fn promotion_rules(&self) -> &[FieldPromotion] {
        &self.rules
    }

    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError> {
        self.extract_with_state(window, &State::new()).await
    }

    async fn extract_with_state(
        &self,
        window: &[TranscriptTurn],
        state: &State,
    ) -> Result<Value, LlmError> {
        let asked = self.asked(state);
        if asked.is_empty() {
            return Ok(json!({}));
        }
        let mut request = DecisionRequest::new(
            self.decision_state(window, state),
            asked
                .iter()
                .map(|a| (a.spec.id.clone(), a.question.clone())),
        );
        request.provider_options = self.provider_options.clone();
        let started = Instant::now();
        let decided = match tokio::time::timeout(self.timeout, self.model.decide(request)).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err(format!("no answer within {} ms", self.timeout.as_millis())),
        };
        let ms = started.elapsed().as_millis() as u64;
        // A failed or slow decision goes to the fallback in full; without
        // one, the extraction fails as an LLM extraction would.
        let response = match decided {
            Ok(response) => response,
            Err(e) if self.fallback.is_some() => {
                tracing::warn!(extractor = %self.name, "decision failed ({e}); asking the fallback");
                let mut failed = super::DecisionResponse::with_answers(
                    self.model.model_id(),
                    std::collections::BTreeMap::new(),
                );
                failed.provider_metadata = json!({ "error": e });
                failed
            }
            Err(e) => return Err(LlmError::Other(format!("decision model: {e}"))),
        };

        let mut result = Map::new();
        let mut uncertain = Vec::new();
        for a in &asked {
            match Self::outcome(a, response.answers.get(&a.spec.id)) {
                Outcome::Decided(v) => {
                    result.insert(a.spec.id.clone(), v);
                }
                Outcome::Uncertain => {
                    result.insert(a.spec.id.clone(), Value::Null);
                    uncertain.push(a);
                }
            }
        }

        let mut fallback = Value::Null;
        if let Some(llm) = &self.fallback {
            // Only questions whose answer would be promoted are worth a
            // language-model call.
            let worth: Vec<&Asked<'_>> = uncertain
                .iter()
                .copied()
                .filter(|a| a.spec.promote.is_some())
                .collect();
            if !worth.is_empty() {
                let started = Instant::now();
                let answered = self.ask_fallback(llm, window, &worth).await;
                let fallback_ms = started.elapsed().as_millis() as u64;
                match answered {
                    Ok(values) => {
                        let mut used = Vec::new();
                        for a in &worth {
                            let Some(v) = values.get(&a.spec.id).filter(|v| !v.is_null()) else {
                                continue;
                            };
                            let write_false =
                                a.spec.promote.as_ref().is_some_and(|p| p.write_false);
                            let decided = match (&a.question, v) {
                                (Question::Boolean { .. }, Value::Bool(false)) if !write_false => {
                                    Value::Null
                                }
                                (Question::Choice { .. }, Value::String(k)) => {
                                    a.values.get(k).cloned().unwrap_or_else(|| v.clone())
                                }
                                _ => v.clone(),
                            };
                            result.insert(a.spec.id.clone(), decided);
                            used.push(a.spec.id.clone());
                        }
                        fallback =
                            json!({ "model": llm.model_id(), "ms": fallback_ms, "answered": used });
                    }
                    Err(e) => {
                        tracing::warn!(extractor = %self.name, "decision fallback failed: {e}");
                        fallback = json!({ "model": llm.model_id(), "ms": fallback_ms, "error": e.to_string() });
                    }
                }
            }
        }

        let uncertain_ids: Vec<&str> = uncertain.iter().map(|a| a.spec.id.as_str()).collect();
        tracing::info!(
            extractor = %self.name,
            model = %response.model,
            ms,
            uncertain = ?uncertain_ids,
            fallback = !fallback.is_null(),
            "decision extractor"
        );
        let answers: Map<String, Value> = response
            .answers
            .iter()
            .map(|(k, a)| (k.clone(), a.to_value()))
            .collect();
        let mut meta = json!({
            "model": response.model,
            "ms": ms,
            "answers": answers,
            "uncertain": uncertain_ids,
        });
        if !response.fallback_triggered_by.is_empty() {
            meta["gateway_fallback"] = json!(response.fallback_triggered_by);
        }
        if let Some(e) = response.provider_metadata.get("error") {
            meta["error"] = e.clone();
        }
        if !fallback.is_null() {
            meta["fallback"] = fallback;
        }
        result.insert("_decision".into(), meta);
        Ok(Value::Object(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::MockDecisionModel;
    use crate::live::extractor::promote_fields;
    use crate::llm::LlmResponse;
    use gemini_genai_rs::prelude::{Content, Part, Role};

    fn turn(user: &str, model: &str) -> TranscriptTurn {
        TranscriptTurn {
            turn_number: 0,
            user: user.into(),
            model: model.into(),
            tool_calls: Vec::new(),
            timestamp: std::time::Instant::now(),
        }
    }

    fn answering(answers: Vec<(&'static str, Answer)>) -> Arc<MockDecisionModel> {
        Arc::new(MockDecisionModel::new(move |_| {
            Ok(answers
                .iter()
                .map(|(k, a)| (k.to_string(), a.clone()))
                .collect())
        }))
    }

    fn confirm() -> DecisionQuestion {
        DecisionQuestion::new(
            "confirmed",
            Question::boolean("Did the caller agree to the booking?"),
        )
        .promote(Promote::to("book_table_confirmed"))
    }

    struct Llm(&'static str, parking_lot::Mutex<usize>);

    #[async_trait]
    impl BaseLlm for Llm {
        fn model_id(&self) -> &str {
            "fallback-llm"
        }
        async fn generate(&self, _: crate::llm::LlmRequest) -> Result<LlmResponse, LlmError> {
            *self.1.lock() += 1;
            Ok(LlmResponse {
                content: Content {
                    role: Some(Role::Model),
                    parts: vec![Part::Text {
                        text: self.0.into(),
                    }],
                },
                finish_reason: Some("STOP".into()),
                usage: None,
            })
        }
    }

    #[tokio::test]
    async fn a_confident_yes_is_promoted() {
        let model = answering(vec![("confirmed", Answer::boolean(0.97))]);
        let ex = DecisionExtractor::new("signals", model.clone(), 2).question(confirm());
        let state = State::new();
        let window = [
            turn("", "Four at seven. Shall I book it?"),
            turn("Yes, book it.", ""),
        ];
        let value = ex.extract_with_state(&window, &state).await.unwrap();
        assert_eq!(value["confirmed"], json!(true));
        assert_eq!(value["_decision"]["uncertain"], json!([]));
        promote_fields(&ex, "signals", &value, &state);
        assert_eq!(state.get::<bool>("book_table_confirmed"), Some(true));
    }

    #[tokio::test]
    async fn the_state_carries_the_conversation_facts_and_stage() {
        let model = answering(vec![("confirmed", Answer::boolean(0.5))]);
        let ex = DecisionExtractor::new("signals", model.clone(), 2)
            .question(confirm())
            .facts(["party_size", "missing"]);
        let state = State::new();
        let _ = state.set("party_size", 4);
        let _ = state.set("flow:active", json!(["confirm"]));
        let mut asked = turn("Four at seven.", "Seven is free. Book it?");
        asked
            .tool_calls
            .push(crate::live::transcript::ToolCallSummary {
                name: "check_availability".into(),
                args_summary: "{}".into(),
                result_summary: "{\"options\":[\"19:00\"]}".into(),
            });
        ex.extract_with_state(&[asked, turn("Yes.", "")], &state)
            .await
            .unwrap();
        let sent = &model.requests()[0];
        assert_eq!(
            sent.state,
            json!({
                "conversation": [
                    { "caller": "Four at seven." },
                    { "tool": "check_availability", "result": "{\"options\":[\"19:00\"]}" },
                    { "agent": "Seven is free. Book it?" },
                    { "caller": "Yes." }
                ],
                "facts": { "party_size": 4 },
                "active_stages": ["confirm"]
            })
        );
    }

    #[tokio::test]
    async fn an_unsure_answer_is_not_promoted_and_a_clear_no_is_not_uncertain() {
        let unsure = answering(vec![("confirmed", Answer::boolean(0.6))]);
        let ex = DecisionExtractor::new("signals", unsure, 2).question(confirm());
        let value = ex.extract(&[turn("Hmm, maybe.", "")]).await.unwrap();
        assert!(value["confirmed"].is_null());
        assert_eq!(value["_decision"]["uncertain"], json!(["confirmed"]));

        let no = answering(vec![("confirmed", Answer::boolean(0.05))]);
        let ex = DecisionExtractor::new("signals", no, 2).question(confirm());
        let value = ex.extract(&[turn("No, wait.", "")]).await.unwrap();
        assert!(
            value["confirmed"].is_null(),
            "true-only: a no is not written"
        );
        assert_eq!(value["_decision"]["uncertain"], json!([]));

        let no = answering(vec![("confirmed", Answer::boolean(0.05))]);
        let ex = DecisionExtractor::new("signals", no, 2).question(
            DecisionQuestion::new("confirmed", Question::boolean("Agreed?"))
                .promote(Promote::to("ok").write_false()),
        );
        let value = ex.extract(&[turn("No.", "")]).await.unwrap();
        assert_eq!(value["confirmed"], json!(false));
    }

    #[tokio::test]
    async fn uncertain_answers_go_to_the_language_model() {
        let model = answering(vec![
            ("confirmed", Answer::boolean(0.55)),
            ("frustration", Answer::score(0.2, 0.9)),
        ]);
        let llm = Arc::new(Llm(r#"{"confirmed": true}"#, parking_lot::Mutex::new(0)));
        let ex = DecisionExtractor::new("signals", model, 2)
            .question(confirm())
            .question(DecisionQuestion::new(
                "frustration",
                Question::score("How frustrated?", ["calm", "upset"]),
            ))
            .with_llm_fallback(llm.clone());
        let value = ex
            .extract(&[turn("Yeah go ahead I guess.", "")])
            .await
            .unwrap();
        assert_eq!(value["confirmed"], json!(true), "the fallback's answer");
        assert_eq!(value["frustration"], json!(0.2));
        assert_eq!(
            value["_decision"]["fallback"]["answered"],
            json!(["confirmed"])
        );
        assert_eq!(*llm.1.lock(), 1);

        // Nothing uncertain: no language-model call.
        let sure = answering(vec![("confirmed", Answer::boolean(0.99))]);
        let llm = Arc::new(Llm("{}", parking_lot::Mutex::new(0)));
        let ex = DecisionExtractor::new("signals", sure, 2)
            .question(confirm())
            .with_llm_fallback(llm.clone());
        ex.extract(&[turn("Yes.", "")]).await.unwrap();
        assert_eq!(*llm.1.lock(), 0);
    }

    #[tokio::test]
    async fn a_slow_or_failed_decision_goes_to_the_fallback() {
        let slow = Arc::new(
            MockDecisionModel::new(|_| {
                Ok([("confirmed".to_string(), Answer::boolean(0.99))].into())
            })
            .with_latency(std::time::Duration::from_millis(200)),
        );
        let llm = Arc::new(Llm(r#"{"confirmed": true}"#, parking_lot::Mutex::new(0)));
        let ex = DecisionExtractor::new("signals", slow.clone(), 2)
            .question(confirm())
            .with_timeout(std::time::Duration::from_millis(20))
            .with_llm_fallback(llm.clone());
        let value = ex.extract(&[turn("Yes.", "")]).await.unwrap();
        assert_eq!(value["confirmed"], json!(true), "the fallback answered");
        assert!(
            value["_decision"]["error"]
                .as_str()
                .unwrap()
                .contains("no answer within 20 ms"),
            "{value}"
        );
        assert_eq!(*llm.1.lock(), 1);

        let failing = Arc::new(MockDecisionModel::new(|_| {
            Err(LlmError::Api {
                status: 503,
                message: "unavailable".into(),
            })
        }));
        // Without a fallback the extraction fails, as an LLM extraction would.
        let ex = DecisionExtractor::new("signals", failing.clone(), 2).question(confirm());
        assert!(ex.extract(&[turn("Yes.", "")]).await.is_err());
        let llm = Arc::new(Llm(r#"{"confirmed": true}"#, parking_lot::Mutex::new(0)));
        let ex = DecisionExtractor::new("signals", failing, 2)
            .question(confirm())
            .with_llm_fallback(llm);
        let value = ex.extract(&[turn("Yes.", "")]).await.unwrap();
        assert_eq!(value["confirmed"], json!(true));
    }

    #[tokio::test]
    async fn a_choice_reads_its_options_from_state() {
        let model = Arc::new(MockDecisionModel::new(|req| {
            let Question::Choice { criteria, .. } = &req.questions["picked"] else {
                panic!("a choice");
            };
            assert_eq!(
                criteria.keys().collect::<Vec<_>>(),
                ["2026-10-20T09:00", "2026-10-21T10:00"]
            );
            Ok([(
                "picked".to_string(),
                Answer::choice("2026-10-20T09:00", 0.92),
            )]
            .into())
        }));
        let ex = DecisionExtractor::new("slots", model, 2).question(
            DecisionQuestion::new(
                "picked",
                Question::choice::<&str, &str>("Which offered time did the caller pick?", []),
            )
            .options_from("availability.slots")
            .promote(Promote::to("slot")),
        );
        let state = State::new();
        let _ = state.set(
            "availability",
            json!({ "slots": ["2026-10-20T09:00", "2026-10-21T10:00"] }),
        );
        let value = ex
            .extract_with_state(&[turn("The first one works.", "")], &state)
            .await
            .unwrap();
        assert_eq!(value["picked"], json!("2026-10-20T09:00"));
        promote_fields(&ex, "slots", &value, &state);
        assert_eq!(
            state.get::<String>("slot").as_deref(),
            Some("2026-10-20T09:00")
        );
    }

    #[tokio::test]
    async fn choosing_none_of_these_decides_nothing() {
        let model = Arc::new(MockDecisionModel::new(|req| {
            let Question::Choice { criteria, .. } = &req.questions["picked"] else {
                panic!("a choice");
            };
            assert_eq!(
                criteria.get(NONE_OPTION).map(String::as_str),
                Some("the caller has not picked a time yet")
            );
            Ok([("picked".to_string(), Answer::choice(NONE_OPTION, 0.95))].into())
        }));
        let ex = DecisionExtractor::new("slots", model, 2).question(
            DecisionQuestion::new("picked", Question::choice::<&str, &str>("Which time?", []))
                .options_from("availability.slots")
                .or_none("the caller has not picked a time yet")
                .promote(Promote::to("slot")),
        );
        let state = State::new();
        let _ = state.set("availability", json!({ "slots": ["09:00", "10:00"] }));
        let value = ex
            .extract_with_state(&[turn("Which ones do you have?", "")], &state)
            .await
            .unwrap();
        assert!(value["picked"].is_null(), "decided: nothing picked");
        assert_eq!(value["_decision"]["uncertain"], json!([]));
        promote_fields(&ex, "slots", &value, &state);
        assert_eq!(state.get_raw("slot"), None);
    }

    #[tokio::test]
    async fn an_object_option_is_promoted_whole() {
        let model = answering(vec![("rx", Answer::choice("rx-2", 0.9))]);
        let ex = DecisionExtractor::new("rx", model, 2).question(
            DecisionQuestion::new(
                "rx",
                Question::choice::<&str, &str>("Which prescription?", []),
            )
            .options_from("prescriptions")
            .promote(Promote::to("chosen")),
        );
        let state = State::new();
        let _ = state.set(
            "prescriptions",
            json!([{ "id": "rx-1", "drug": "Metformin" }, { "id": "rx-2", "drug": "Lisinopril" }]),
        );
        let value = ex
            .extract_with_state(&[turn("The Lisinopril.", "")], &state)
            .await
            .unwrap();
        assert_eq!(value["rx"], json!({ "id": "rx-2", "drug": "Lisinopril" }));
    }

    #[tokio::test]
    async fn a_question_is_asked_only_in_its_stages() {
        let model = answering(vec![("next", Answer::choice("book", 0.9))]);
        let ex = DecisionExtractor::new("route", model.clone(), 2).question(
            DecisionQuestion::new(
                "next",
                Question::choice("What next?", [("book", "confirmed"), ("stay", "not yet")]),
            )
            .active_in(["confirm"])
            .promote(Promote::to("route")),
        );
        let state = State::new();
        let _ = state.set("flow:active", json!(["collect"]));
        let value = ex
            .extract_with_state(&[turn("Hi.", "")], &state)
            .await
            .unwrap();
        assert_eq!(value, json!({}));
        assert!(model.requests().is_empty(), "no call when nothing applies");

        let _ = state.set("flow:active", json!(["confirm"]));
        let value = ex
            .extract_with_state(&[turn("Yes.", "")], &state)
            .await
            .unwrap();
        assert_eq!(value["next"], json!("book"));
    }

    #[tokio::test]
    async fn a_low_certainty_choice_and_a_refusal_are_uncertain() {
        let model = answering(vec![
            ("next", Answer::choice("book", 0.4)),
            ("intent", Answer::Refusal(json!({ "type": "refusal" }))),
        ]);
        let ex = DecisionExtractor::new("route", model, 2)
            .question(DecisionQuestion::new(
                "next",
                Question::choice("What next?", [("book", "b"), ("stay", "s")]),
            ))
            .question(DecisionQuestion::new(
                "intent",
                Question::boolean("Wants a person?"),
            ));
        let value = ex.extract(&[turn("Uh.", "")]).await.unwrap();
        assert_eq!(value["_decision"]["uncertain"], json!(["next", "intent"]));
    }

    #[test]
    fn promotion_rules_name_the_written_keys() {
        let ex = DecisionExtractor::new("signals", answering(vec![]), 2)
            .question(confirm())
            .question(DecisionQuestion::new("unpromoted", Question::boolean("X?")));
        let keys: Vec<&str> = ex
            .promotion_rules()
            .iter()
            .map(|r| r.state_key.as_str())
            .collect();
        assert_eq!(keys, ["book_table_confirmed"]);
    }

    #[test]
    fn a_turn_without_the_caller_is_skipped() {
        let ex = DecisionExtractor::new("signals", answering(vec![]), 2).question(confirm());
        assert!(!ex.should_extract(&[turn("Four.", "Checking."), turn("", "Seven is free.")]));
        assert!(ex.should_extract(&[turn("", "Seven is free."), turn("Book it.", "")]));
    }
}
