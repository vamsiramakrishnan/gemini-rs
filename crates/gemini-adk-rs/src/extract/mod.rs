//! The extraction kit — `Extract` records with deterministic recognizers.
//!
//! An [`Extract`] record declares typed fields, each filled by a [`Recognizer`]
//! that reads the conversation transcript on the CPU — no model, no network, no
//! accelerator. It compiles to a [`TurnExtractor`] (so it plugs into the
//! existing extraction pipeline) and promotes recognized fields into governed
//! `State`, where `Flow` guards (`done(captured([...]))`) and repair read them.
//!
//! This is the deterministic, transcript-sourced slice of the kit. LLM / fetch
//! / MCP / agent *resolvers* and the `#[derive(Extract)]` macro layer on top of
//! this same record model.
//!
//! ```
//! use gemini_adk_rs::extract::{Extract, Recognizer};
//!
//! let order = Extract::record("order")
//!     .field("quantity", Recognizer::integer())
//!     .field("item", Recognizer::one_of(["pizza", "salad", "soda"]))
//!     .field("confirmed", Recognizer::yes_no())
//!     .window(3)
//!     .build();
//! ```

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dashmap::DashMap;
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

use crate::AsyncSourceFn;
use crate::live::extractor::{ExtractionTrigger, FieldPromotion, OnComplete, TurnExtractor};
use crate::live::transcript::TranscriptTurn;
use crate::llm::LlmError;
use crate::orchestration::AgentMode;
use crate::state::State;
use crate::text::TextAgent;

static MONEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$?\s?(\d{1,3}(?:,\d{3})*|\d+)(?:\.(\d{1,2}))?").unwrap());
static INT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-?\d+").unwrap());
static DATE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d{4}-\d{2}-\d{2}\b").unwrap());
// 12-hour clock with an am/pm marker: "3pm", "3:30 pm".
static TIME12_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(\d{1,2})(?::(\d{2}))?\s*([ap]m)\b").unwrap());
// 24-hour clock: "15:00", "09:30".
static TIME24_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([01]?\d|2[0-3]):([0-5]\d)\b").unwrap());

/// Normalize a clock time found in `text` to a 24-hour `"HH:MM"` string.
fn parse_time(text: &str) -> Option<String> {
    if let Some(c) = TIME12_RE.captures(text) {
        let mut hour: u32 = c.get(1)?.as_str().parse().ok()?;
        let min: u32 = c.get(2).map(|m| m.as_str()).unwrap_or("00").parse().ok()?;
        let pm = c.get(3)?.as_str().eq_ignore_ascii_case("pm");
        if hour > 12 {
            return None; // "13pm" is not a real time
        }
        if pm && hour < 12 {
            hour += 12;
        } else if !pm && hour == 12 {
            hour = 0; // 12am -> 00:00
        }
        return Some(format!("{hour:02}:{min:02}"));
    }
    let c = TIME24_RE.captures(text)?;
    Some(format!(
        "{:02}:{}",
        c.get(1)?.as_str().parse::<u32>().ok()?,
        c.get(2)?.as_str()
    ))
}

/// A deterministic transcript recognizer: `text -> (value, confidence)`.
///
/// Recognizers run on the CPU over the user transcript. Confidence is in
/// `0.0..=1.0`; deterministic matches are high-confidence, fuzzy matches carry
/// their similarity score.
#[derive(Clone)]
pub enum Recognizer {
    /// First integer in the text. If `near` is non-empty, at least one anchor
    /// word must be present for the match to count.
    Integer {
        /// Anchor words that must appear for the integer to be recognized.
        near: Vec<String>,
    },
    /// A monetary amount (`$1,250.00`, `200`) → JSON number.
    Money,
    /// First capture group (or whole match) of a regex → string.
    Regex(Regex),
    /// The first option that appears (case-insensitive substring) → string.
    OneOf(Vec<String>),
    /// The option with the best Jaro-Winkler similarity ≥ `min` → string.
    /// Useful for ASR-mangled names matched against a roster.
    Fuzzy {
        /// Candidate values to match against.
        options: Vec<String>,
        /// Minimum similarity in `0.0..=1.0`.
        min: f64,
    },
    /// Affirmative/negative detection → boolean.
    YesNo,
    /// A calendar/clock expression → a JSON object with any of the keys
    /// `date` (`YYYY-MM-DD`), `time` (24h `HH:MM`), `day` (`today`/`tomorrow`/
    /// `tonight`/`yesterday`), `weekday`, and `part` (`morning`/`afternoon`/
    /// `evening`/`noon`/`midnight`). Deterministic, on-device — a small
    /// Duckling-style normalizer, not a full grammar.
    DateTime,
}

impl Recognizer {
    /// First integer, optionally anchored to nearby words.
    pub fn integer() -> Self {
        Recognizer::Integer { near: Vec::new() }
    }
    /// Integer recognized only when one of `anchors` is present.
    pub fn integer_near<I, S>(anchors: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Recognizer::Integer {
            near: anchors.into_iter().map(Into::into).collect(),
        }
    }
    /// A monetary amount.
    pub fn money() -> Self {
        Recognizer::Money
    }
    /// A regex (first capture group, or the whole match).
    pub fn regex(pattern: &str) -> Self {
        Recognizer::Regex(Regex::new(pattern).expect("invalid recognizer regex"))
    }
    /// Match against a fixed set of options (case-insensitive substring).
    pub fn one_of<I, S>(options: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Recognizer::OneOf(options.into_iter().map(Into::into).collect())
    }
    /// Fuzzy-match against options with a default 0.85 threshold.
    pub fn fuzzy<I, S>(options: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Recognizer::Fuzzy {
            options: options.into_iter().map(Into::into).collect(),
            min: 0.85,
        }
    }
    /// Affirmative/negative.
    pub fn yes_no() -> Self {
        Recognizer::YesNo
    }
    /// A calendar/clock expression normalized to a JSON object.
    pub fn datetime() -> Self {
        Recognizer::DateTime
    }

    /// Recognize a value in `text`, with a confidence in `0.0..=1.0`.
    pub fn recognize(&self, text: &str) -> Option<(Value, f32)> {
        let lower = text.to_lowercase();
        match self {
            Recognizer::Integer { near } => {
                if !near.is_empty() && !near.iter().any(|a| lower.contains(&a.to_lowercase())) {
                    return None;
                }
                let m = INT_RE.find(text)?;
                m.as_str()
                    .parse::<i64>()
                    .ok()
                    .map(|n| (Value::from(n), 1.0))
            }
            Recognizer::Money => {
                let caps = MONEY_RE.captures(text)?;
                let whole = caps.get(1)?.as_str().replace(',', "");
                let cents = caps.get(2).map(|c| c.as_str()).unwrap_or("0");
                let amount: f64 = format!("{whole}.{cents:0<2}").parse().ok()?;
                Some((Value::from(amount), 1.0))
            }
            Recognizer::Regex(re) => {
                let caps = re.captures(text)?;
                let s = caps.get(1).or_else(|| caps.get(0))?.as_str().to_string();
                Some((Value::from(s), 1.0))
            }
            Recognizer::OneOf(options) => options
                .iter()
                .find(|o| lower.contains(&o.to_lowercase()))
                .map(|o| (Value::from(o.clone()), 1.0)),
            Recognizer::Fuzzy { options, min } => {
                let mut best: Option<(&String, f64)> = None;
                for opt in options {
                    let ol = opt.to_lowercase();
                    // Best similarity of the option against the whole text and each word.
                    let sim = std::iter::once(lower.as_str())
                        .chain(lower.split_whitespace())
                        .map(|w| strsim::jaro_winkler(&ol, w))
                        .fold(0.0_f64, f64::max);
                    if sim >= *min && best.map(|(_, b)| sim > b).unwrap_or(true) {
                        best = Some((opt, sim));
                    }
                }
                best.map(|(opt, sim)| (Value::from(opt.clone()), sim as f32))
            }
            Recognizer::YesNo => {
                // Whole-word tokens (keeping apostrophes so "don't" stays intact),
                // so "incorrect" doesn't match "correct" and "another" doesn't match
                // "no". Negation is checked first: "not correct"/"don't confirm" → false.
                const NO: &[&str] = &[
                    "no",
                    "nope",
                    "nah",
                    "not",
                    "don't",
                    "dont",
                    "doesn't",
                    "isn't",
                    "won't",
                    "never",
                    "incorrect",
                    "wrong",
                    "negative",
                ];
                const YES: &[&str] = &[
                    "yes",
                    "yeah",
                    "yep",
                    "yup",
                    "sure",
                    "correct",
                    "confirm",
                    "confirmed",
                    "ok",
                    "okay",
                    "right",
                    "affirmative",
                ];
                let tokens: Vec<&str> = lower
                    .split(|c: char| !(c.is_alphanumeric() || c == '\''))
                    .filter(|t| !t.is_empty())
                    .collect();
                if tokens.iter().any(|t| NO.contains(t)) {
                    Some((Value::Bool(false), 0.9))
                } else if tokens.iter().any(|t| YES.contains(t)) {
                    Some((Value::Bool(true), 0.9))
                } else {
                    None
                }
            }
            Recognizer::DateTime => {
                const WEEKDAYS: &[&str] = &[
                    "monday",
                    "tuesday",
                    "wednesday",
                    "thursday",
                    "friday",
                    "saturday",
                    "sunday",
                ];
                let mut obj = serde_json::Map::new();
                if let Some(m) = DATE_RE.find(text) {
                    obj.insert("date".into(), Value::from(m.as_str().to_string()));
                }
                if let Some(t) = parse_time(&lower) {
                    obj.insert("time".into(), Value::from(t));
                }
                if let Some(d) = ["today", "tomorrow", "tonight", "yesterday"]
                    .into_iter()
                    .find(|d| lower.contains(d))
                {
                    obj.insert("day".into(), Value::from(d));
                }
                if let Some(w) = WEEKDAYS.iter().find(|w| lower.contains(*w)) {
                    obj.insert("weekday".into(), Value::from(*w));
                }
                if let Some(p) = ["morning", "afternoon", "evening", "noon", "midnight"]
                    .into_iter()
                    .find(|p| lower.contains(p))
                {
                    obj.insert("part".into(), Value::from(p));
                }
                if obj.is_empty() {
                    None
                } else {
                    Some((Value::Object(obj), 1.0))
                }
            }
        }
    }
}

/// How a field is filled.
#[derive(Clone)]
enum Source {
    /// Deterministic transcript recognizer (sync, no state).
    Recognize(Recognizer),
    /// Async resolver: bind `args` from `State`, fetch, optionally cache.
    Resolve {
        /// State keys bound into the args object passed to the fetcher.
        args: Vec<String>,
        /// Optional cache time-to-live keyed by `(field, canonical args)`.
        ttl: Option<Duration>,
        /// The async fetcher.
        /// (An [`AsyncSourceFn`] bound from the args object, not the whole `State`.)
        fetch: AsyncSourceFn<Value>,
    },
}

/// Post-recognition validator: a recognized value is only promoted when this
/// returns `true`.
type FieldValidator = Arc<dyn Fn(&Value) -> bool + Send + Sync>;

/// A field in an [`Extract`] record.
#[derive(Clone)]
pub struct Field {
    name: String,
    source: Source,
    state_key: String,
    overwrite: bool,
    /// Optional predicate; a recognized value failing it is rejected (not promoted).
    validate: Option<FieldValidator>,
}

/// A declarative extraction record: typed fields filled by recognizers and/or
/// async resolvers.
#[derive(Clone)]
pub struct Extract {
    name: String,
    fields: Vec<Field>,
    window: usize,
    trigger: ExtractionTrigger,
    on_complete: Option<OnComplete>,
}

impl Extract {
    /// Start building a record with the given extractor name.
    pub fn record(name: impl Into<String>) -> ExtractBuilder {
        ExtractBuilder {
            name: name.into(),
            fields: Vec::new(),
            window: 3,
            trigger: ExtractionTrigger::EveryTurn,
            on_complete: None,
        }
    }

    /// Compile into a [`TurnExtractor`] for registration.
    pub fn into_extractor(self) -> Arc<dyn TurnExtractor> {
        Arc::new(RecordExtractor::new(self))
    }

    /// The `(field name, state key)` pairs this record promotes. Callers that run
    /// the extractor and promote the returned record into `State` themselves (e.g.
    /// the simulation harness) use this to map each field to its state key.
    pub fn field_state_keys(&self) -> Vec<(String, String)> {
        self.fields
            .iter()
            .map(|f| (f.name.clone(), f.state_key.clone()))
            .collect()
    }
}

/// Builder for an [`Extract`] record.
pub struct ExtractBuilder {
    name: String,
    fields: Vec<Field>,
    window: usize,
    trigger: ExtractionTrigger,
    on_complete: Option<OnComplete>,
}

impl ExtractBuilder {
    /// Add a field filled by `recognizer`, promoted to a state key of the same name.
    pub fn field(mut self, name: impl Into<String>, recognizer: Recognizer) -> Self {
        let name = name.into();
        self.fields.push(Field {
            state_key: name.clone(),
            name,
            source: Source::Recognize(recognizer),
            overwrite: false,
            validate: None,
        });
        self
    }
    /// Add a field promoted to a custom state key.
    pub fn field_to(
        mut self,
        name: impl Into<String>,
        state_key: impl Into<String>,
        recognizer: Recognizer,
    ) -> Self {
        self.fields.push(Field {
            name: name.into(),
            state_key: state_key.into(),
            source: Source::Recognize(recognizer),
            overwrite: false,
            validate: None,
        });
        self
    }
    /// Add a field filled by an **async resolver** — a tool call, HTTP fetch, or
    /// MCP request. `args` names the `State` keys bound into the JSON object
    /// passed to `fetch`; the returned value becomes the field. With a `ttl`,
    /// results are memoized by `(field, canonical args)` for that duration.
    pub fn field_resolve<I, S, F, Fut>(
        mut self,
        name: impl Into<String>,
        args: I,
        ttl: Option<Duration>,
        fetch: F,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        let name = name.into();
        let fetch = Arc::new(fetch);
        self.fields.push(Field {
            state_key: name.clone(),
            name,
            source: Source::Resolve {
                args: args.into_iter().map(Into::into).collect(),
                ttl,
                fetch: Arc::new(move |a| {
                    let fetch = fetch.clone();
                    Box::pin(async move { fetch(a).await })
                }),
            },
            overwrite: false,
            validate: None,
        });
        self
    }
    /// Attach a validator to the **most recently added** field: a recognized
    /// value is promoted only when `predicate` returns `true` (else it is
    /// rejected, as if no value was recognized this turn).
    pub fn validate<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&Value) -> bool + Send + Sync + 'static,
    {
        if let Some(field) = self.fields.last_mut() {
            field.validate = Some(Arc::new(predicate));
        }
        self
    }

    /// Number of recent turns the recognizers read (default 3).
    pub fn window(mut self, n: usize) -> Self {
        self.window = n;
        self
    }
    /// When the record runs (default `EveryTurn`).
    pub fn trigger(mut self, trigger: ExtractionTrigger) -> Self {
        self.trigger = trigger;
        self
    }
    /// Run `agent` (in `mode`) when this record lands fields in state — the
    /// `on_complete(dispatch(agent))` effect. Its result lands in `{name}:result`.
    pub fn on_complete(mut self, agent: Arc<dyn TextAgent>, mode: AgentMode) -> Self {
        self.on_complete = Some(OnComplete { agent, mode });
        self
    }
    /// Finalize the record.
    pub fn build(self) -> Extract {
        Extract {
            name: self.name,
            fields: self.fields,
            window: self.window,
            trigger: self.trigger,
            on_complete: self.on_complete,
        }
    }
}

/// A [`TurnExtractor`] that runs an [`Extract`] record's recognizers and
/// resolvers, and promotes the recognized fields into state.
pub struct RecordExtractor {
    spec: Extract,
    promotions: Vec<FieldPromotion>,
    /// Per-field resolver cache keyed by `(field, canonical args)`.
    cache: Arc<DashMap<String, (Value, Instant)>>,
}

impl RecordExtractor {
    /// Build from a record spec.
    pub fn new(spec: Extract) -> Self {
        let promotions = spec
            .fields
            .iter()
            .map(|f| {
                let p = if f.overwrite {
                    FieldPromotion::overwrite(&f.name)
                } else {
                    FieldPromotion::keep_known(&f.name)
                };
                p.to(&f.state_key)
            })
            .collect();
        Self {
            spec,
            promotions,
            cache: Arc::new(DashMap::new()),
        }
    }

    /// Resolve one async field, honoring the per-field TTL cache.
    ///
    /// Args bind from `fresh` (values recognized in *this* turn, keyed by their
    /// state key) first, then from session `State` — so a resolver sees a slot
    /// recognized in the same utterance, not a stale value.
    async fn resolve_field(
        &self,
        field: &str,
        args: &[String],
        ttl: Option<Duration>,
        fetch: &AsyncSourceFn<Value>,
        fresh: &serde_json::Map<String, Value>,
        state: &State,
    ) -> Option<Value> {
        // Bind args, preferring this turn's recognitions (skip absent keys).
        let mut obj = serde_json::Map::new();
        for key in args {
            if let Some(v) = fresh.get(key).cloned().or_else(|| state.get::<Value>(key)) {
                obj.insert(key.clone(), v);
            }
        }
        let args_value = Value::Object(obj);
        let cache_key = format!("{field}|{args_value}");
        if let Some(ttl) = ttl
            && let Some(entry) = self.cache.get(&cache_key)
            && entry.1.elapsed() < ttl
        {
            return Some(entry.0.clone());
        }
        match fetch(args_value).await {
            Ok(value) => {
                if ttl.is_some() {
                    self.cache
                        .insert(cache_key, (value.clone(), Instant::now()));
                }
                Some(value)
            }
            Err(e) => {
                tracing::warn!(field, "resolver failed: {e}");
                None
            }
        }
    }
}

#[async_trait]
impl TurnExtractor for RecordExtractor {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn window_size(&self) -> usize {
        self.spec.window
    }

    fn trigger(&self) -> ExtractionTrigger {
        self.spec.trigger.clone()
    }

    fn promotion_rules(&self) -> &[FieldPromotion] {
        &self.promotions
    }

    fn on_complete(&self) -> Option<OnComplete> {
        self.spec.on_complete.clone()
    }

    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError> {
        // Recognizer-only path (no State): used by callers that don't bind args.
        let text = window
            .iter()
            .map(|t| t.user.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let mut obj = serde_json::Map::new();
        for field in &self.spec.fields {
            if let Source::Recognize(rec) = &field.source
                && let Some((value, _confidence)) = rec.recognize(&text)
            {
                if field.validate.as_ref().is_some_and(|v| !v(&value)) {
                    continue; // recognized but rejected by the slot validator
                }
                obj.insert(field.name.clone(), value);
            }
        }
        Ok(Value::Object(obj))
    }

    async fn extract_with_state(
        &self,
        window: &[TranscriptTurn],
        state: &State,
    ) -> Result<Value, LlmError> {
        let text = window
            .iter()
            .map(|t| t.user.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let mut obj = serde_json::Map::new();
        // Sync recognizers over the transcript. `fresh` maps each recognized
        // field's *state key* to its value, so resolvers can bind args from
        // values recognized in this same turn (before promotion runs).
        let mut fresh = serde_json::Map::new();
        for field in &self.spec.fields {
            if let Source::Recognize(rec) = &field.source
                && let Some((value, confidence)) = rec.recognize(&text)
            {
                if field.validate.as_ref().is_some_and(|v| !v(&value)) {
                    continue; // recognized but rejected by the slot validator
                }
                // Record provenance + confidence under the `state_meta:` convention
                // so `State::evidence()` can surface how a slot was filled.
                let _ = state.set(
                    format!("state_meta:{}", field.state_key),
                    serde_json::json!({ "source": "extraction", "confidence": confidence }),
                );
                fresh.insert(field.state_key.clone(), value.clone());
                obj.insert(field.name.clone(), value);
            }
        }
        // Async resolvers, bound from this turn's recognitions + State.
        let resolves = self
            .spec
            .fields
            .iter()
            .filter_map(|field| match &field.source {
                Source::Resolve { args, ttl, fetch } => Some(async {
                    self.resolve_field(&field.name, args, *ttl, fetch, &fresh, state)
                        .await
                        .map(|v| (field.name.clone(), v))
                }),
                Source::Recognize(_) => None,
            });
        for resolved in futures_util::future::join_all(resolves)
            .await
            .into_iter()
            .flatten()
        {
            obj.insert(resolved.0, resolved.1);
        }
        Ok(Value::Object(obj))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn turn(user: &str) -> TranscriptTurn {
        TranscriptTurn {
            turn_number: 0,
            user: user.to_string(),
            model: String::new(),
            tool_calls: Vec::new(),
            timestamp: std::time::Instant::now(),
        }
    }

    #[test]
    fn recognizers_basic() {
        assert_eq!(
            Recognizer::integer()
                .recognize("I want 3 of them")
                .unwrap()
                .0,
            json!(3)
        );
        assert_eq!(
            Recognizer::money()
                .recognize("that'll be $1,250.50")
                .unwrap()
                .0,
            json!(1250.50)
        );
        assert_eq!(
            Recognizer::one_of(["pizza", "salad"])
                .recognize("a large PIZZA please")
                .unwrap()
                .0,
            json!("pizza")
        );
        assert_eq!(
            Recognizer::yes_no()
                .recognize("yes that's right")
                .unwrap()
                .0,
            json!(true)
        );
        assert_eq!(
            Recognizer::yes_no().recognize("no thanks").unwrap().0,
            json!(false)
        );
        assert!(Recognizer::yes_no().recognize("maybe later").is_none());
    }

    #[test]
    fn yes_no_negation_wins_and_is_word_aware() {
        let r = Recognizer::yes_no();
        // Negation beats an affirmative substring.
        assert_eq!(r.recognize("not correct").unwrap().0, json!(false));
        assert_eq!(r.recognize("don't confirm that").unwrap().0, json!(false));
        // "incorrect" must not match "correct"; it's a negation.
        assert_eq!(r.recognize("that's incorrect").unwrap().0, json!(false));
        // Word-aware: "another" does not contain a standalone "no".
        assert!(r.recognize("another option").is_none());
        // Plain affirmation still works.
        assert_eq!(r.recognize("yes please").unwrap().0, json!(true));
    }

    #[tokio::test]
    async fn resolver_binds_args_recognized_this_turn() {
        // `slot` is recognized this turn (not yet in State); the resolver must
        // bind it from the same utterance, not see it missing.
        let spec = Extract::record("booking")
            .field("slot", Recognizer::one_of(["morning", "afternoon"]))
            .field_resolve("availability", ["slot"], None, |args: Value| async move {
                Ok(json!({ "slot_seen": args.get("slot").cloned() }))
            })
            .build();
        let ext = RecordExtractor::new(spec);
        let state = State::new(); // slot is NOT in State yet
        let out = ext
            .extract_with_state(&[turn("afternoon works")], &state)
            .await
            .unwrap();
        assert_eq!(out["slot"], json!("afternoon"));
        assert_eq!(out["availability"], json!({ "slot_seen": "afternoon" }));
    }

    #[test]
    fn datetime_normalizes_clock_and_calendar() {
        let r = Recognizer::datetime();
        // 12-hour clock with pm.
        assert_eq!(
            r.recognize("can we meet at 3pm").unwrap().0,
            json!({"time": "15:00"})
        );
        // Minutes + am, plus a relative day.
        assert_eq!(
            r.recognize("tomorrow at 9:30 am works").unwrap().0,
            json!({"time": "09:30", "day": "tomorrow"})
        );
        // 12am normalizes to midnight.
        assert_eq!(
            r.recognize("12am sharp").unwrap().0,
            json!({"time": "00:00"})
        );
        // 24-hour clock + weekday + part of day + ISO date.
        assert_eq!(
            r.recognize("friday afternoon, 2026-06-05 at 15:00")
                .unwrap()
                .0,
            json!({"date": "2026-06-05", "time": "15:00", "weekday": "friday", "part": "afternoon"})
        );
        // A bare integer is not a time.
        assert!(r.recognize("a table for 4 people").is_none());
        // "13pm" is not a real clock time.
        assert!(r.recognize("at 13pm").is_none());
    }

    #[test]
    fn integer_near_anchors() {
        let r = Recognizer::integer_near(["quantity", "want"]);
        assert_eq!(r.recognize("I want 5").unwrap().0, json!(5));
        assert!(r.recognize("call me at 5").is_none()); // no anchor word
    }

    #[test]
    fn fuzzy_matches_misheard_name() {
        let r = Recognizer::fuzzy(["Johnson", "Jackson", "Jensen"]);
        let (v, conf) = r.recognize("the name is jonson").unwrap();
        assert_eq!(v, json!("Johnson"));
        assert!(conf > 0.85);
    }

    #[tokio::test]
    async fn record_extractor_captures_fields() {
        let spec = Extract::record("order")
            .field("quantity", Recognizer::integer_near(["want", "get"]))
            .field("item", Recognizer::one_of(["pizza", "salad", "soda"]))
            .window(2)
            .build();
        let extractor = RecordExtractor::new(spec);
        assert_eq!(extractor.name(), "order");
        assert_eq!(extractor.window_size(), 2);
        assert_eq!(extractor.promotion_rules().len(), 2);

        let window = vec![turn("I want 2 large pizza")];
        let out = extractor.extract(&window).await.unwrap();
        assert_eq!(out["quantity"], json!(2));
        assert_eq!(out["item"], json!("pizza"));
    }

    #[tokio::test]
    async fn record_resolves_async_field_from_state() {
        let spec = Extract::record("booking")
            .field("slot", Recognizer::one_of(["morning", "afternoon"]))
            .field_resolve("availability", ["slot"], None, |args: Value| async move {
                let slot = args.get("slot").and_then(|v| v.as_str()).unwrap_or("");
                Ok(serde_json::json!({ "open": slot == "afternoon" }))
            })
            .build();
        let ext = RecordExtractor::new(spec);
        let state = State::new();
        let _ = state.set("slot", "afternoon");
        let out = ext
            .extract_with_state(&[turn("afternoon please")], &state)
            .await
            .unwrap();
        assert_eq!(out["slot"], json!("afternoon"));
        assert_eq!(out["availability"], json!({ "open": true }));
    }

    #[tokio::test]
    async fn resolver_field_caches_within_ttl() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let spec = Extract::record("b")
            .field_resolve("v", ["k"], Some(Duration::from_secs(60)), move |_args| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(json!("x"))
                }
            })
            .build();
        let ext = RecordExtractor::new(spec);
        let state = State::new();
        let _ = state.set("k", 1);
        let _ = ext.extract_with_state(&[turn("a")], &state).await.unwrap();
        let _ = ext.extract_with_state(&[turn("a")], &state).await.unwrap();
        // Identical args within the TTL → fetched once.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn on_complete_is_exposed() {
        use crate::error::AgentError;
        struct A;
        #[async_trait]
        impl TextAgent for A {
            fn name(&self) -> &str {
                "a"
            }
            async fn run(&self, _s: &State) -> Result<String, AgentError> {
                Ok("done".into())
            }
        }
        let spec = Extract::record("x")
            .field("q", Recognizer::integer())
            .on_complete(Arc::new(A), AgentMode::Dispatch)
            .build();
        assert!(RecordExtractor::new(spec).on_complete().is_some());
    }

    #[tokio::test]
    async fn record_extractor_omits_unrecognized() {
        let spec = Extract::record("order")
            .field("item", Recognizer::one_of(["pizza"]))
            .build();
        let out = RecordExtractor::new(spec)
            .extract(&[turn("hello there")])
            .await
            .unwrap();
        assert!(out.as_object().unwrap().is_empty());
    }
}
