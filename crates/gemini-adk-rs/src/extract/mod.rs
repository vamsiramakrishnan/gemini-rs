//! The extraction kit — `Extract` records with deterministic recognizers.
//!
//! An [`Extract`] record declares typed fields, each filled by a [`Recognizer`]
//! that reads the conversation transcript on the CPU — no model, no network, no
//! accelerator. It compiles to a [`TurnExtractor`] (so it plugs into the
//! existing extraction pipeline) and promotes recognized fields into governed
//! `State`, where `Flow` guards (`done(captured([...]))`) and repair read them.
//!
//! This is the deterministic, transcript-sourced slice of the kit (see the
//! extraction-kit RFC). LLM / fetch / MCP / agent *resolvers* and the
//! `#[derive(Extract)]` macro layer on top of this same record model.
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

use std::sync::Arc;

use async_trait::async_trait;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

use crate::live::extractor::{ExtractionTrigger, FieldPromotion, TurnExtractor};
use crate::live::transcript::TranscriptTurn;
use crate::llm::LlmError;

static MONEY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\$?\s?(\d{1,3}(?:,\d{3})*|\d+)(?:\.(\d{1,2}))?").unwrap());
static INT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"-?\d+").unwrap());
static DATE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b\d{4}-\d{2}-\d{2}\b").unwrap());
// 12-hour clock with an am/pm marker: "3pm", "3:30 pm".
static TIME12_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b(\d{1,2})(?::(\d{2}))?\s*([ap]m)\b").unwrap());
// 24-hour clock: "15:00", "09:30".
static TIME24_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b([01]?\d|2[0-3]):([0-5]\d)\b").unwrap());

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
                const YES: &[&str] = &["yes", "yeah", "yep", "sure", "correct", "confirm", "ok"];
                const NO: &[&str] = &["no", "nope", "nah", "incorrect", "don't", "do not"];
                if YES.iter().any(|w| lower.contains(w)) {
                    Some((Value::Bool(true), 0.9))
                } else if NO.iter().any(|w| lower.contains(w)) {
                    Some((Value::Bool(false), 0.9))
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

/// A field in an [`Extract`] record.
#[derive(Clone)]
pub struct Field {
    name: String,
    recognizer: Recognizer,
    state_key: String,
    overwrite: bool,
}

/// A declarative extraction record: typed fields filled by recognizers.
#[derive(Clone)]
pub struct Extract {
    name: String,
    fields: Vec<Field>,
    window: usize,
    trigger: ExtractionTrigger,
}

impl Extract {
    /// Start building a record with the given extractor name.
    pub fn record(name: impl Into<String>) -> ExtractBuilder {
        ExtractBuilder {
            name: name.into(),
            fields: Vec::new(),
            window: 3,
            trigger: ExtractionTrigger::EveryTurn,
        }
    }

    /// Compile into a [`TurnExtractor`] for registration.
    pub fn into_extractor(self) -> Arc<dyn TurnExtractor> {
        Arc::new(RecordExtractor::new(self))
    }
}

/// Builder for an [`Extract`] record.
pub struct ExtractBuilder {
    name: String,
    fields: Vec<Field>,
    window: usize,
    trigger: ExtractionTrigger,
}

impl ExtractBuilder {
    /// Add a field filled by `recognizer`, promoted to a state key of the same name.
    pub fn field(mut self, name: impl Into<String>, recognizer: Recognizer) -> Self {
        let name = name.into();
        self.fields.push(Field {
            state_key: name.clone(),
            name,
            recognizer,
            overwrite: false,
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
            recognizer,
            overwrite: false,
        });
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
    /// Finalize the record.
    pub fn build(self) -> Extract {
        Extract {
            name: self.name,
            fields: self.fields,
            window: self.window,
            trigger: self.trigger,
        }
    }
}

/// A [`TurnExtractor`] that runs an [`Extract`] record's recognizers over the
/// transcript and promotes recognized fields into state.
pub struct RecordExtractor {
    spec: Extract,
    promotions: Vec<FieldPromotion>,
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
        Self { spec, promotions }
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

    async fn extract(&self, window: &[TranscriptTurn]) -> Result<Value, LlmError> {
        // Recognizers read the user's speech across the window.
        let text = window
            .iter()
            .map(|t| t.user.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let mut obj = serde_json::Map::new();
        for field in &self.spec.fields {
            if let Some((value, _confidence)) = field.recognizer.recognize(&text) {
                obj.insert(field.name.clone(), value);
            }
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
