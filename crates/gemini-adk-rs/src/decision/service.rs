//! [`Decisions`]: a bank of questions asked of a decision model against the
//! rolling conversation.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Answer, DecisionModel, DecisionRequest, Question};
use crate::flow::{DECISION_TURN_KEY, DecisionRecord, DecisionScope, Outcome, decision_key};
use crate::live::transcript::TranscriptTurn;
use crate::state::State;

/// The `P(true)` a boolean answer needs to count as yes (and `1 -` this to
/// count as no), unless the question sets its own.
pub const DEFAULT_BOOLEAN_THRESHOLD: f64 = 0.85;
/// The certainty a choice or score answer needs, unless the question sets
/// its own.
pub const DEFAULT_CERTAINTY_THRESHOLD: f64 = 0.6;
/// How long a round waits for the decision model.
pub const DEFAULT_DECISION_TIMEOUT: Duration = Duration::from_secs(2);
/// How many turns of the conversation the decision model reads.
pub const DEFAULT_HISTORY_TURNS: usize = 40;
/// The option a choice gets from [`Decision::or_none`].
pub const NONE_OPTION: &str = "none_of_these";

/// One question in the bank.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// The question.
    pub question: Question,
    /// For a choice: a state path (`key` or `key.field.0`) holding the
    /// options, read at each ask. Each array element is an option: a string,
    /// a number, or an object keyed by its `id`, `value` or `name` field.
    pub options_from: Option<String>,
    /// For a choice: add [`NONE_OPTION`] with this description. A choice
    /// always picks something, so a question like "which offered time did
    /// the caller pick?" needs a way to say they have not.
    pub none: Option<String>,
    /// The `P(true)` (boolean) or certainty (choice, score) an answer needs.
    pub at_least: Option<f64>,
    /// Write the decided value into this state key: the chosen option (an
    /// object option whole), the score, or the boolean. The question is then
    /// also asked whenever an active guard reads that key.
    pub writes: Option<String>,
}

impl Decision {
    /// A question with the defaults.
    pub fn new(question: Question) -> Self {
        Self {
            question,
            options_from: None,
            none: None,
            at_least: None,
            writes: None,
        }
    }

    /// Read a choice's options from a state path.
    #[must_use]
    pub fn options_from(mut self, path: impl Into<String>) -> Self {
        self.options_from = Some(path.into());
        self
    }

    /// Add a "none of these" option meaning `description`.
    #[must_use]
    pub fn or_none(mut self, description: impl Into<String>) -> Self {
        self.none = Some(description.into());
        self
    }

    /// Require this probability (boolean) or certainty (choice, score).
    #[must_use]
    pub fn at_least(mut self, threshold: f64) -> Self {
        self.at_least = Some(threshold);
        self
    }

    /// Write the decided value into `key`.
    #[must_use]
    pub fn writes(mut self, key: impl Into<String>) -> Self {
        self.writes = Some(key.into());
        self
    }

    /// Record `answer` to question `id` for caller turn `turn` as if a
    /// decision model gave it with full certainty, writing what a live ask
    /// writes: `true`/`false` for a boolean, an option's key for a choice
    /// ([`NONE_OPTION`] for none), a number for a score, anything else for
    /// unsure. For scripted conversations: offline scenarios and tests.
    pub fn record_scripted(&self, id: &str, answer: &Value, state: &State, turn: &str) {
        let (_, values) = resolve(self, state);
        let answer = match answer {
            Value::Bool(b) => Some(Answer::boolean(if *b { 1.0 } else { 0.0 })),
            Value::String(o) => Some(Answer::choice(o.clone(), 1.0)),
            Value::Number(n) => n.as_f64().map(|n| Answer::score(n, 1.0)),
            _ => None,
        };
        let (outcome, value, confidence, write) = decide(self, answer.as_ref(), &values);
        record(state, id, outcome, value, confidence, turn);
        if let (Some(key), Some(write)) = (&self.writes, write) {
            let _ = state.set(key, write);
        }
    }

    fn threshold(&self) -> f64 {
        let default = match self.question {
            Question::Boolean { .. } => DEFAULT_BOOLEAN_THRESHOLD,
            _ => DEFAULT_CERTAINTY_THRESHOLD,
        };
        self.at_least.unwrap_or(default)
    }
}

/// The conversation a decision model reads: caller and agent lines and tool
/// results in order, the facts the active stages ground the model in, and
/// which stages are active.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversation {
    entries: Vec<Value>,
    context: Vec<String>,
    active_stages: Vec<String>,
    turn: String,
}

impl Conversation {
    /// The conversation in `turns`, oldest first. Each turn is the caller's
    /// words, then its tool calls, then the agent's reply.
    pub fn from_turns(turns: &[TranscriptTurn]) -> Self {
        let mut entries = Vec::new();
        let mut turn = String::new();
        for t in turns {
            let caller = t.user.trim();
            if !caller.is_empty() {
                entries.push(json!({ "caller": caller }));
                let mut h = std::collections::hash_map::DefaultHasher::new();
                caller.hash(&mut h);
                turn = format!("{}:{:x}", t.turn_number, h.finish());
            }
            for call in &t.tool_calls {
                let result = serde_json::from_str::<Value>(&call.result_summary)
                    .unwrap_or_else(|_| Value::String(call.result_summary.clone()));
                entries.push(json!({ "tool": call.name, "result": result }));
            }
            let agent = t.model.trim();
            if !agent.is_empty() {
                entries.push(json!({ "agent": agent }));
            }
        }
        Self {
            entries,
            turn,
            ..Self::default()
        }
    }

    /// Add the facts the active stages ground the model in.
    #[must_use]
    pub fn with_context<S: Into<String>>(mut self, lines: impl IntoIterator<Item = S>) -> Self {
        self.context = lines.into_iter().map(Into::into).collect();
        self
    }

    /// Add the active stages' ids.
    #[must_use]
    pub fn with_active_stages<S: Into<String>>(
        mut self,
        stages: impl IntoIterator<Item = S>,
    ) -> Self {
        self.active_stages = stages.into_iter().map(Into::into).collect();
        self
    }

    /// The caller turn the latest caller words belong to: the turn's number
    /// and a hash of the words. Empty when the caller has not spoken.
    pub fn turn(&self) -> &str {
        &self.turn
    }

    /// Make this conversation's caller turn the one answers count for, so an
    /// answer about an earlier turn stops satisfying guards. Call it at every
    /// decision point, including one where nothing is asked.
    pub fn begin_turn(&self, state: &State) {
        if state.get::<String>(DECISION_TURN_KEY).as_deref() != Some(self.turn.as_str()) {
            let _ = state.set(DECISION_TURN_KEY, self.turn.clone());
        }
    }

    /// Whether the caller has said anything.
    pub fn has_caller(&self) -> bool {
        !self.turn.is_empty()
    }

    /// The state sent to the decision model.
    pub fn to_state(&self) -> Value {
        let mut out = json!({ "conversation": self.entries });
        if !self.context.is_empty() {
            out["context"] = json!(self.context);
        }
        if !self.active_stages.is_empty() {
            out["active_stages"] = json!(self.active_stages);
        }
        out
    }
}

/// What one ask did.
#[derive(Debug, Clone, Default)]
pub struct Round {
    /// Questions sent to the model.
    pub asked: Vec<String>,
    /// Questions already answered for this caller turn.
    pub reused: Vec<String>,
    /// How long the model took.
    pub latency: Duration,
    /// Why the model could not answer. Its questions were recorded unsure.
    pub error: Option<String>,
}

/// A bank of questions asked of a decision model against the rolling
/// conversation.
///
/// Guards name the questions (`{"decided": "confirmed"}`, see
/// [`crate::flow::decided`]); the runtime asks the ones the flow can act on at
/// each decision point, in one request, and records each answer under
/// `decision:{id}` with the caller turn it is about. An answer counts only
/// for that turn, so nothing is latched and nothing has to be cleared when
/// the caller corrects themselves: the next ask reads the correction.
///
/// ```
/// use std::sync::Arc;
/// use gemini_adk_rs::decision::{Answer, Decision, Decisions, MockDecisionModel, Question};
///
/// let model = Arc::new(MockDecisionModel::new(|_| {
///     Ok([("confirmed".to_string(), Answer::boolean(0.97))].into())
/// }));
/// let decisions = Decisions::new(model).question(
///     "confirmed",
///     Decision::new(Question::boolean(
///         "In their last turn, did the caller agree to the booking that was read back?",
///     )),
/// );
/// assert!(decisions.get("confirmed").is_some());
/// ```
pub struct Decisions {
    model: Arc<dyn DecisionModel>,
    bank: BTreeMap<String, Decision>,
    standing: BTreeSet<String>,
    history_turns: usize,
    timeout: Duration,
}

impl std::fmt::Debug for Decisions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decisions")
            .field("model", &self.model.model_id())
            .field("questions", &self.bank.keys().collect::<Vec<_>>())
            .field("standing", &self.standing)
            .field("history_turns", &self.history_turns)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Decisions {
    /// An empty bank over `model`.
    pub fn new(model: Arc<dyn DecisionModel>) -> Self {
        Self {
            model,
            bank: BTreeMap::new(),
            standing: BTreeSet::new(),
            history_turns: DEFAULT_HISTORY_TURNS,
            timeout: DEFAULT_DECISION_TIMEOUT,
        }
    }

    /// Declare a question.
    #[must_use]
    pub fn question(mut self, id: impl Into<String>, decision: Decision) -> Self {
        self.bank.insert(id.into(), decision);
        self
    }

    /// Ask these questions at every caller turn, whatever the flow needs:
    /// questions read outside the flow, by phase transitions or patterns.
    #[must_use]
    pub fn standing<S: Into<String>>(mut self, ids: impl IntoIterator<Item = S>) -> Self {
        self.standing.extend(ids.into_iter().map(Into::into));
        self
    }

    /// How many turns the decision model reads (default 40).
    #[must_use]
    pub fn history_turns(mut self, turns: usize) -> Self {
        self.history_turns = turns.max(1);
        self
    }

    /// How long an ask waits for the model (default 2 s). A slow or failed
    /// ask records its questions unsure, so guards on them do not hold.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The declared questions.
    pub fn bank(&self) -> &BTreeMap<String, Decision> {
        &self.bank
    }

    /// One declared question.
    pub fn get(&self, id: &str) -> Option<&Decision> {
        self.bank.get(id)
    }

    /// How many turns the decision model reads.
    pub fn history_len(&self) -> usize {
        self.history_turns
    }

    /// The model's identifier.
    pub fn model_id(&self) -> &str {
        self.model.model_id()
    }

    /// The questions worth asking for `scope`: the declared ones it names,
    /// the standing ones, and those that write a key it reads.
    pub fn select(&self, scope: &DecisionScope) -> BTreeSet<String> {
        self.bank
            .iter()
            .filter(|(id, d)| {
                scope.questions.contains(*id)
                    || self.standing.contains(*id)
                    || d.writes.as_ref().is_some_and(|k| scope.reads.contains(k))
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Ask `ids` about `conversation` and record the answers in `state`.
    ///
    /// A question already answered in `state` for the conversation's caller
    /// turn is not asked again; one recorded unsure because the model could
    /// not be asked (or had no options to offer) is. With no caller words
    /// yet, nothing is asked. The bank holds no session state, so one bank
    /// can serve many sessions.
    pub async fn ask(
        &self,
        ids: &BTreeSet<String>,
        conversation: &Conversation,
        state: &State,
    ) -> Round {
        let mut round = Round::default();
        let turn = conversation.turn().to_string();
        conversation.begin_turn(state);
        if !conversation.has_caller() {
            return round;
        }

        let mut questions = BTreeMap::new();
        let mut options: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
        for id in ids {
            let Some(decision) = self.bank.get(id) else {
                continue;
            };
            let answered = DecisionRecord::current(state, id)
                .is_some_and(|r| r.outcome != Outcome::Unsure || r.confidence.is_some());
            if answered {
                round.reused.push(id.clone());
                continue;
            }
            let (question, values) = resolve(decision, state);
            if question.validate().is_err() {
                // No options yet (nothing offered): unsure, not stale.
                record(state, id, Outcome::Unsure, Value::Null, None, &turn);
                continue;
            }
            questions.insert(id.clone(), question);
            options.insert(id.clone(), values);
        }
        if questions.is_empty() {
            return round;
        }
        round.asked = questions.keys().cloned().collect();

        let started = Instant::now();
        let request = DecisionRequest {
            state: conversation.to_state(),
            questions,
            provider_options: None,
        };
        let response = tokio::time::timeout(self.timeout, self.model.decide(request)).await;
        round.latency = started.elapsed();
        let response = match response {
            Ok(Ok(r)) => r,
            failed => {
                let error = match failed {
                    Ok(Err(e)) => e.to_string(),
                    _ => format!("no answer within {} ms", self.timeout.as_millis()),
                };
                tracing::warn!(model = %self.model.model_id(), asked = ?round.asked, "decision round failed: {error}");
                for id in &round.asked {
                    record(state, id, Outcome::Unsure, Value::Null, None, &turn);
                }
                round.error = Some(error);
                return round;
            }
        };

        let mut outcomes = Vec::new();
        for id in &round.asked {
            let decision = &self.bank[id];
            let (outcome, value, confidence, write) =
                decide(decision, response.answers.get(id), &options[id]);
            record(state, id, outcome, value, confidence, &turn);
            if let (Some(key), Some(write)) = (&decision.writes, write) {
                let _ = state.set(key, write);
            }
            outcomes.push(format!("{id}={outcome:?}"));
        }
        tracing::info!(
            model = %response.model,
            ms = round.latency.as_millis() as u64,
            outcomes = %outcomes.join(" "),
            "decisions"
        );
        round
    }
}

/// The question as asked now (options read from state, "none of these"
/// added) and each option key's value.
fn resolve(decision: &Decision, state: &State) -> (Question, BTreeMap<String, Value>) {
    let mut question = decision.question.clone();
    let mut values = BTreeMap::new();
    if let Question::Choice { criteria, .. } = &mut question {
        if let Some(path) = &decision.options_from {
            let found = read_path(state, path)
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();
            if !found.is_empty() {
                criteria.clear();
            }
            for option in found.into_iter().take(super::MAX_CHOICE_OPTIONS) {
                let key = option_key(&option);
                let description = match &option {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                criteria.insert(key.clone(), description);
                values.insert(key, option);
            }
        }
        if let Some(none) = &decision.none
            && !criteria.is_empty()
        {
            criteria.insert(NONE_OPTION.to_string(), none.clone());
        }
    }
    (question, values)
}

/// How an answer comes out: the outcome, the recorded value, the confidence,
/// and the value to write.
fn decide(
    decision: &Decision,
    answer: Option<&Answer>,
    values: &BTreeMap<String, Value>,
) -> (Outcome, Value, Option<f64>, Option<Value>) {
    let t = decision.threshold();
    match answer {
        Some(Answer::Boolean { probability: p }) => {
            if *p >= t {
                (Outcome::Yes, Value::Null, Some(*p), Some(json!(true)))
            } else if *p <= 1.0 - t {
                (Outcome::No, Value::Null, Some(*p), Some(json!(false)))
            } else {
                (Outcome::Unsure, Value::Null, Some(*p), None)
            }
        }
        Some(a @ Answer::Choice { choice, .. }) => {
            let c = a.certainty();
            if c.unwrap_or(0.0) < t {
                (Outcome::Unsure, json!(choice), c, None)
            } else if choice == NONE_OPTION {
                (Outcome::None, Value::Null, c, None)
            } else {
                let write = values.get(choice).cloned().unwrap_or_else(|| json!(choice));
                (Outcome::Chosen, json!(choice), c, Some(write))
            }
        }
        Some(a @ Answer::Score { score, .. }) => {
            let c = a.certainty();
            if c.unwrap_or(0.0) >= t {
                (Outcome::Scored, json!(score), c, Some(json!(score)))
            } else {
                (Outcome::Unsure, json!(score), c, None)
            }
        }
        Some(Answer::Refusal(_) | Answer::Other(_)) | None => {
            (Outcome::Unsure, Value::Null, None, None)
        }
    }
}

fn record(
    state: &State,
    id: &str,
    outcome: Outcome,
    value: Value,
    confidence: Option<f64>,
    turn: &str,
) {
    let record = DecisionRecord {
        outcome,
        value,
        confidence,
        turn: turn.to_string(),
    };
    if let Ok(v) = serde_json::to_value(record) {
        let _ = state.set(decision_key(id), v);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::MockDecisionModel;
    use crate::flow::{Decided, DecisionRecord, Expect};

    fn turn(n: u32, user: &str, model: &str) -> TranscriptTurn {
        TranscriptTurn {
            turn_number: n,
            user: user.into(),
            model: model.into(),
            tool_calls: Vec::new(),
            timestamp: std::time::Instant::now(),
        }
    }

    fn holds(state: &State, q: &str, expect: Expect) -> bool {
        Decided {
            question: q.into(),
            expect,
        }
        .holds(state)
    }

    fn answering(answers: Vec<(&'static str, Answer)>) -> Arc<MockDecisionModel> {
        Arc::new(MockDecisionModel::new(move |_| {
            Ok(answers
                .iter()
                .map(|(k, a)| (k.to_string(), a.clone()))
                .collect())
        }))
    }

    fn ids(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(ToString::to_string).collect()
    }

    fn confirm() -> Decision {
        Decision::new(Question::boolean("Did the caller agree to the booking?"))
    }

    #[tokio::test]
    async fn an_answer_is_recorded_for_its_turn_and_asked_once_per_turn() {
        let model = answering(vec![("confirmed", Answer::boolean(0.97))]);
        let decisions = Decisions::new(model.clone()).question("confirmed", confirm());
        let state = State::new();
        let convo = Conversation::from_turns(&[
            turn(0, "", "Four at seven under Rossi. Shall I book it?"),
            turn(1, "Yes, book it.", ""),
        ]);
        let round = decisions.ask(&ids(&["confirmed"]), &convo, &state).await;
        assert_eq!(round.asked, ["confirmed"]);
        assert!(holds(&state, "confirmed", Expect::Yes));

        let again = decisions.ask(&ids(&["confirmed"]), &convo, &state).await;
        assert_eq!(
            again.reused,
            ["confirmed"],
            "same caller turn: not asked again"
        );
        assert_eq!(model.requests().len(), 1);

        // The caller speaks again: the old yes no longer counts until asked.
        let next = Conversation::from_turns(&[
            turn(1, "Yes, book it.", "Before I do, any allergies?"),
            turn(2, "Hmm, wait.", ""),
        ]);
        let _ = state.set(DECISION_TURN_KEY, next.turn());
        assert!(!holds(&state, "confirmed", Expect::Yes));
    }

    #[tokio::test]
    async fn the_model_reads_the_whole_conversation_with_context() {
        let model = answering(vec![("confirmed", Answer::boolean(0.5))]);
        let decisions = Decisions::new(model.clone()).question("confirmed", confirm());
        let state = State::new();
        let mut asked = turn(1, "Four at seven.", "Seven is free. Book it?");
        asked
            .tool_calls
            .push(crate::live::transcript::ToolCallSummary {
                name: "check_availability".into(),
                args_summary: "{}".into(),
                result_summary: r#"{"options":["19:00"]}"#.into(),
            });
        let convo = Conversation::from_turns(&[turn(0, "", "Hello."), asked, turn(2, "Yes.", "")])
            .with_context(["Party of 4 at 19:00."])
            .with_active_stages(["confirm"]);
        decisions.ask(&ids(&["confirmed"]), &convo, &state).await;
        assert_eq!(
            model.requests()[0].state,
            json!({
                "conversation": [
                    { "agent": "Hello." },
                    { "caller": "Four at seven." },
                    { "tool": "check_availability", "result": { "options": ["19:00"] } },
                    { "agent": "Seven is free. Book it?" },
                    { "caller": "Yes." }
                ],
                "context": ["Party of 4 at 19:00."],
                "active_stages": ["confirm"]
            })
        );
        let record: DecisionRecord = state.get(&decision_key("confirmed")).unwrap();
        assert_eq!(record.outcome, Outcome::Unsure, "0.5 is neither yes nor no");
    }

    #[tokio::test]
    async fn a_pick_writes_its_option_and_none_writes_nothing() {
        let model = answering(vec![("slot", Answer::choice("2026-10-20T09:00", 0.9))]);
        let decisions = Decisions::new(model).question(
            "slot",
            Decision::new(Question::choice::<&str, &str>("Which offered time?", []))
                .options_from("availability.slots")
                .or_none("not picked yet")
                .writes("slot_choice"),
        );
        let state = State::new();
        let _ = state.set(
            "availability",
            json!({ "slots": ["2026-10-20T09:00", "2026-10-21T10:00"] }),
        );
        let convo = Conversation::from_turns(&[turn(0, "The first one.", "")]);
        decisions.ask(&ids(&["slot"]), &convo, &state).await;
        assert_eq!(
            state.get_raw("slot_choice"),
            Some(json!("2026-10-20T09:00"))
        );
        assert!(holds(&state, "slot", Expect::Is("2026-10-20T09:00".into())));

        let model = answering(vec![("slot", Answer::choice(NONE_OPTION, 0.95))]);
        let decisions = Decisions::new(model).question(
            "slot",
            Decision::new(Question::choice::<&str, &str>("Which offered time?", []))
                .options_from("availability.slots")
                .or_none("not picked yet")
                .writes("other"),
        );
        let state = State::new();
        let _ = state.set(
            "availability",
            json!({ "slots": ["2026-10-20T09:00", "2026-10-21T10:00"] }),
        );
        decisions.ask(&ids(&["slot"]), &convo, &state).await;
        assert_eq!(state.get_raw("other"), None);
        assert!(holds(&state, "slot", Expect::No));
    }

    #[tokio::test]
    async fn without_options_a_pick_is_unsure_and_not_asked() {
        let model = answering(vec![]);
        let decisions = Decisions::new(model.clone()).question(
            "slot",
            Decision::new(Question::choice::<&str, &str>("Which offered time?", []))
                .options_from("availability.slots"),
        );
        let state = State::new();
        let convo = Conversation::from_turns(&[turn(0, "Something next week.", "")]);
        let round = decisions.ask(&ids(&["slot"]), &convo, &state).await;
        assert!(round.asked.is_empty());
        assert!(model.requests().is_empty());
        let record: DecisionRecord = state.get(&decision_key("slot")).unwrap();
        assert_eq!(record.outcome, Outcome::Unsure);
    }

    #[tokio::test]
    async fn a_failed_round_fails_closed() {
        let failing = Arc::new(MockDecisionModel::new(|_| {
            Err(crate::llm::LlmError::Api {
                status: 503,
                message: "busy".into(),
            })
        }));
        let decisions = Decisions::new(failing).question("confirmed", confirm());
        let state = State::new();
        let convo = Conversation::from_turns(&[turn(0, "Yes.", "")]);
        let round = decisions.ask(&ids(&["confirmed"]), &convo, &state).await;
        assert!(round.error.is_some());
        assert!(!holds(&state, "confirmed", Expect::Yes));
        assert!(!holds(&state, "confirmed", Expect::No));

        let slow = Arc::new(
            MockDecisionModel::new(|_| {
                Ok([("confirmed".to_string(), Answer::boolean(0.99))].into())
            })
            .with_latency(Duration::from_millis(200)),
        );
        let decisions = Decisions::new(slow)
            .question("confirmed", confirm())
            .with_timeout(Duration::from_millis(20));
        let round = decisions.ask(&ids(&["confirmed"]), &convo, &state).await;
        assert!(round.error.unwrap().contains("no answer within 20 ms"));
        assert!(!holds(&state, "confirmed", Expect::Yes));

        // A question the model could not answer is asked again on the same
        // turn, at the next decision point.
        let model = answering(vec![("confirmed", Answer::boolean(0.97))]);
        let decisions = Decisions::new(model).question("confirmed", confirm());
        let round = decisions.ask(&ids(&["confirmed"]), &convo, &state).await;
        assert_eq!(round.asked, ["confirmed"]);
        assert!(holds(&state, "confirmed", Expect::Yes));
    }

    #[test]
    fn a_new_caller_turn_retires_earlier_answers_even_when_nothing_is_asked() {
        let state = State::new();
        let first = Conversation::from_turns(&[turn(0, "Yes.", "")]);
        first.begin_turn(&state);
        record(
            &state,
            "confirmed",
            Outcome::Yes,
            Value::Null,
            None,
            first.turn(),
        );
        assert!(holds(&state, "confirmed", Expect::Yes));

        let next = Conversation::from_turns(&[turn(0, "Yes.", "Booked?"), turn(1, "Wait.", "")]);
        next.begin_turn(&state);
        assert!(!holds(&state, "confirmed", Expect::Yes));

        // The same turn again writes nothing.
        let cursor = state.mutation_cursor();
        next.begin_turn(&state);
        assert_eq!(state.mutation_cursor(), cursor);
    }

    #[tokio::test]
    async fn one_bank_serves_many_sessions() {
        let model = Arc::new(MockDecisionModel::new(|request| {
            let said = request.state["conversation"][0]["caller"].as_str() == Some("Yes.");
            Ok([(
                "confirmed".to_string(),
                Answer::boolean(if said { 0.97 } else { 0.03 }),
            )]
            .into())
        }));
        let decisions = Decisions::new(model.clone()).question("confirmed", confirm());
        let (a, b) = (State::new(), State::new());
        let yes = Conversation::from_turns(&[turn(0, "Yes.", "")]);
        let no = Conversation::from_turns(&[turn(0, "No.", "")]);
        decisions.ask(&ids(&["confirmed"]), &yes, &a).await;
        let round = decisions.ask(&ids(&["confirmed"]), &no, &b).await;
        assert_eq!(round.asked, ["confirmed"], "b's turn is its own");
        assert!(holds(&a, "confirmed", Expect::Yes));
        assert!(holds(&b, "confirmed", Expect::No));
        assert_eq!(model.requests().len(), 2);
    }

    #[tokio::test]
    async fn a_score_split_between_adjacent_levels_is_decided() {
        let split = Answer::Score {
            score: 2.5,
            probabilities: [("0", 0.0), ("1", 0.0), ("2", 0.5), ("3", 0.5)]
                .into_iter()
                .map(|(k, p)| (k.to_string(), p))
                .collect(),
            confidence: Some(0.5),
        };
        let model = answering(vec![("anger", split)]);
        let decisions = Decisions::new(model).question(
            "anger",
            Decision::new(Question::score(
                "How angry?",
                ["calm", "a bit", "upset", "angry"],
            )),
        );
        let state = State::new();
        let convo = Conversation::from_turns(&[turn(0, "This is the third time!", "")]);
        decisions.ask(&ids(&["anger"]), &convo, &state).await;
        assert!(holds(&state, "anger", Expect::AtLeast(2.0)));
    }

    #[test]
    fn selection_takes_named_standing_and_written_questions() {
        let decisions = Decisions::new(answering(vec![]))
            .question("confirmed", confirm())
            .question("anger", Decision::new(Question::score("?", ["a", "b"])))
            .question(
                "slot",
                Decision::new(Question::choice("?", [("a", "a")])).writes("slot_choice"),
            )
            .question("unused", confirm())
            .standing(["anger"]);
        let scope = DecisionScope {
            questions: ids(&["confirmed", "unknown"]),
            reads: ids(&["slot_choice"]),
        };
        assert_eq!(
            decisions.select(&scope),
            ids(&["anger", "confirmed", "slot"])
        );
    }
}
