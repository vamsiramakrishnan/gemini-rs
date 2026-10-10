//! The authoring interface a coding harness uses to build a session spec.
//!
//! A harness (a coding agent, an IDE, a CLI wizard) edits `agent.json` on the
//! user's behalf. These operations tell it what can be written, what is wrong
//! with the document, and which decisions are still the user's to make:
//!
//! - [`catalog`] lists the vocabulary: voices, guard atoms, policies, tool
//!   bindings, resume policies, question rules and diagnostic codes.
//! - [`check`] validates a document and reports structured [`Diagnostic`]s.
//!   Each has a JSON pointer and, where the repair is mechanical, a [`Fix`]
//!   expressed as JSON-patch operations.
//! - [`plan`] lists the open decisions as [`Question`]s. Every option carries
//!   the patch that records it.
//! - [`answer`] applies chosen options. It plans again before each answer,
//!   so every patch is computed against the current document.
//! - [`apply_patch`] applies JSON-patch operations (RFC 6902 `add`,
//!   `replace`, `remove`) atomically.
//!
//! All of them take the document as a JSON value, so a document that does
//! not deserialize still gets diagnostics.
//!
//! Fixes in one [`CheckReport`] are each computed against the checked
//! document. Apply one, then check again: two fixes from the same report can
//! touch the same array.
//!
//! ```
//! use gemini_adk_fluent_rs::spec::authoring::{self, Answer, Decisions};
//! use serde_json::json;
//!
//! let doc = json!({ "name": "", "instruction": "Answer questions about opening hours." });
//! let plan = authoring::plan(&doc, &Decisions::new());
//! assert_eq!(plan.questions[0].id, "name");
//!
//! let answered = authoring::answer(
//!     &doc,
//!     &[Answer::with_value("name", json!("hours-line"))],
//!     &Decisions::new(),
//! )
//! .unwrap();
//! assert_eq!(answered.spec["name"], "hours-line");
//! ```

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{MEMORY_TOOL_NAMES, RUNTIME_WRITTEN, SessionSpec, levenshtein};
use crate::conversation::StageSpec;
use crate::policy::Policy;

/// Version of the authoring interface. Bumped when an operation's output
/// changes incompatibly.
pub const AUTHORING_VERSION: u32 = 1;

/// The placeholder an option's patch uses for the user's value. A string
/// equal to it is replaced by [`Answer::value`].
pub const ANSWER_PLACEHOLDER: &str = "$answer";

/// Name of the extractor that question and fix patches add boolean caller
/// signals to (confirmations, intents).
pub const SIGNALS_EXTRACTOR: &str = "caller_signals";

// ---------------------------------------------------------------------------
// JSON patch
// ---------------------------------------------------------------------------

/// One JSON-patch operation (RFC 6902 subset). `path` is a JSON pointer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PatchOp {
    /// Insert into an array (`/-` appends) or set an object member.
    Add {
        /// Target location.
        path: String,
        /// Value to add.
        value: Value,
    },
    /// Replace an existing value.
    Replace {
        /// Target location; must exist.
        path: String,
        /// Replacement value.
        value: Value,
    },
    /// Remove an existing value.
    Remove {
        /// Target location; must exist.
        path: String,
    },
}

impl PatchOp {
    /// The operation's target pointer.
    pub fn path(&self) -> &str {
        match self {
            PatchOp::Add { path, .. }
            | PatchOp::Replace { path, .. }
            | PatchOp::Remove { path } => path,
        }
    }
}

/// Why a patch could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchError {
    /// Index of the failing operation.
    pub index: usize,
    /// Its target pointer.
    pub path: String,
    /// What went wrong.
    pub reason: String,
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "patch operation {} at '{}': {}",
            self.index, self.path, self.reason
        )
    }
}

impl std::error::Error for PatchError {}

/// Apply `ops` in order to a copy of `doc`. Either every operation applies
/// or the document is returned unchanged inside the error.
pub fn apply_patch(doc: &Value, ops: &[PatchOp]) -> Result<Value, PatchError> {
    let mut out = doc.clone();
    for (index, op) in ops.iter().enumerate() {
        apply_op(&mut out, op).map_err(|reason| PatchError {
            index,
            path: op.path().to_string(),
            reason,
        })?;
    }
    Ok(out)
}

fn apply_op(doc: &mut Value, op: &PatchOp) -> Result<(), String> {
    let path = op.path();
    if !path.is_empty() && !path.starts_with('/') {
        return Err("a JSON pointer must be empty or start with '/'".into());
    }
    match op {
        PatchOp::Replace { value, .. } => {
            let target = doc
                .pointer_mut(path)
                .ok_or_else(|| "no value at this path".to_string())?;
            *target = value.clone();
            Ok(())
        }
        PatchOp::Add { value, .. } => {
            if path.is_empty() {
                *doc = value.clone();
                return Ok(());
            }
            let (parent, last) = split_pointer(path);
            match doc.pointer_mut(parent) {
                Some(Value::Object(map)) => {
                    map.insert(last, value.clone());
                    Ok(())
                }
                Some(Value::Array(items)) => {
                    if last == "-" {
                        items.push(value.clone());
                        return Ok(());
                    }
                    let index = array_index(&last)?;
                    if index > items.len() {
                        return Err(format!("index {index} is past the end of the array"));
                    }
                    items.insert(index, value.clone());
                    Ok(())
                }
                Some(_) => Err("the parent is not an object or array".into()),
                None => Err("the parent does not exist".into()),
            }
        }
        PatchOp::Remove { .. } => {
            if path.is_empty() {
                return Err("cannot remove the whole document".into());
            }
            let (parent, last) = split_pointer(path);
            match doc.pointer_mut(parent) {
                Some(Value::Object(map)) => map
                    .remove(&last)
                    .map(|_| ())
                    .ok_or_else(|| "no value at this path".to_string()),
                Some(Value::Array(items)) => {
                    let index = array_index(&last)?;
                    if index >= items.len() {
                        return Err("no value at this path".into());
                    }
                    items.remove(index);
                    Ok(())
                }
                _ => Err("no value at this path".into()),
            }
        }
    }
}

/// Split a non-empty pointer into its (still escaped) parent and its
/// unescaped last token.
fn split_pointer(path: &str) -> (&str, String) {
    let cut = path.rfind('/').unwrap_or(0);
    let last = path[cut + 1..].replace("~1", "/").replace("~0", "~");
    (&path[..cut], last)
}

fn array_index(token: &str) -> Result<usize, String> {
    if token.len() > 1 && token.starts_with('0') {
        return Err(format!("'{token}' is not a valid array index"));
    }
    token
        .parse()
        .map_err(|_| format!("'{token}' is not a valid array index"))
}

/// Escape one reference token for a JSON pointer.
fn escape(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// An `add` that appends `value` to the array at `array`, creating the array
/// when it does not exist yet.
fn append(doc: &Value, array: &str, value: Value) -> PatchOp {
    if doc.pointer(array).is_some_and(Value::is_array) {
        PatchOp::Add {
            path: format!("{array}/-"),
            value,
        }
    } else {
        PatchOp::Add {
            path: array.to_string(),
            value: json!([value]),
        }
    }
}

fn add(path: impl Into<String>, value: Value) -> PatchOp {
    PatchOp::Add {
        path: path.into(),
        value,
    }
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// One authorable item: its name, what it means and an example.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CatalogEntry {
    /// Name as written in the spec (or the id pattern, for questions).
    pub name: String,
    /// What it does.
    pub meaning: String,
    /// A valid example, when one applies.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub example: Value,
}

/// The vocabulary a harness may write. The full document shape is the spec's
/// JSON Schema (`SessionSpec::json_schema`, `adk spec schema`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Catalog {
    /// [`AUTHORING_VERSION`].
    pub version: u32,
    /// Output modalities.
    pub modalities: Vec<String>,
    /// Prebuilt voice names. Other names are passed through to the provider.
    pub voices: Vec<String>,
    /// Guard atoms and combinators.
    pub guards: Vec<CatalogEntry>,
    /// Conversation policies.
    pub policies: Vec<CatalogEntry>,
    /// Ways to implement a declared tool.
    pub tool_bindings: Vec<CatalogEntry>,
    /// What happens when a digression closes.
    pub resume: Vec<CatalogEntry>,
    /// Question rules [`plan`] can raise, by id pattern.
    pub questions: Vec<CatalogEntry>,
    /// Diagnostic codes [`check`] can report.
    pub diagnostics: Vec<CatalogEntry>,
}

fn entry(name: &str, meaning: &str, example: Value) -> CatalogEntry {
    CatalogEntry {
        name: name.into(),
        meaning: meaning.into(),
        example,
    }
}

const VOICES: [&str; 5] = ["Aoede", "Charon", "Fenrir", "Kore", "Puck"];

/// The authoring vocabulary.
pub fn catalog() -> Catalog {
    Catalog {
        version: AUTHORING_VERSION,
        modalities: vec!["text".into(), "audio".into()],
        voices: VOICES.iter().map(|v| (*v).to_string()).collect(),
        guards: vec![
            entry(
                "always",
                "Always holds. Rejected as a commit guard.",
                json!("always"),
            ),
            entry(
                "is_true",
                "The state key is exactly true.",
                json!({ "is_true": "user_confirmed" }),
            ),
            entry(
                "is_set",
                "The state key has a value.",
                json!({ "is_set": "party_size" }),
            ),
            entry(
                "eq",
                "The state key equals the value.",
                json!({ "eq": ["tier", "gold"] }),
            ),
            entry(
                "captured",
                "Every listed state key has a value.",
                json!({ "captured": ["party_size", "slot"] }),
            ),
            entry(
                "called_ok",
                "The tool has run successfully.",
                json!({ "called_ok": "book_table" }),
            ),
            entry(
                "done",
                "The stage is complete.",
                json!({ "done": "collect" }),
            ),
            entry(
                "all",
                "Every guard holds.",
                json!({ "all": [{ "is_set": "slot" }, { "is_true": "user_confirmed" }] }),
            ),
            entry(
                "any",
                "At least one guard holds.",
                json!({ "any": [{ "is_true": "intent:cancel" }, { "is_true": "intent:human_agent" }] }),
            ),
            entry(
                "not",
                "The guard does not hold.",
                json!({ "not": { "is_true": "large_party" } }),
            ),
        ],
        policies: vec![
            entry(
                "redact",
                "Redact these state keys in the journal, persistence snapshots and \
                 extraction events. Transcript text is not covered.",
                json!({ "kind": "redact", "keys": ["card_number"] }),
            ),
            entry(
                "safety_handoff",
                "End the conversation when any `intent:{name}` flag becomes true. \
                 Something must write the flag, such as a signal extractor. Its \
                 digression restricts no tools on the turn it is entered; a handoff \
                 digression whose stage admits only a transfer tool does not have that gap.",
                json!({ "kind": "safety_handoff", "intents": ["human_agent"] }),
            ),
            entry(
                "commit",
                "Idempotency and compensation for a committing tool.",
                json!({
                    "kind": "commit",
                    "tool": "book_table",
                    "idempotency_key": "{booking_ref}",
                    "compensate_with": "cancel_booking"
                }),
            ),
        ],
        tool_bindings: vec![
            entry(
                "stub",
                "No binding. Codegen writes a typed stub that you implement.",
                json!({
                    "name": "check_availability",
                    "description": "Check table availability for a date and time.",
                    "parameters": {
                        "type": "object",
                        "properties": { "datetime": { "type": "string" } },
                        "required": ["datetime"]
                    }
                }),
            ),
            entry(
                "mock",
                "A canned response and state writes, for offline runs and demos.",
                json!({
                    "name": "check_availability",
                    "response": { "available": true },
                    "set_state": { "availability_checked": true }
                }),
            ),
            entry(
                "http",
                "Call an HTTP endpoint with the arguments.",
                json!({
                    "name": "book_table",
                    "http": { "method": "POST", "url": "https://bookings.example.com/v1/book" }
                }),
            ),
            entry(
                "mcp",
                "Call the tool of the same name on an MCP server (a command line or an \
                 http(s) URL).",
                json!({ "name": "book_table", "mcp": "python tools/server.py" }),
            ),
        ],
        resume: vec![
            entry(
                "previous",
                "The layer beneath continues where it was suspended (default).",
                json!("previous"),
            ),
            entry(
                "restart",
                "The layer beneath restarts its stages. Filled slots stay filled.",
                json!("restart"),
            ),
            entry(
                "terminate",
                "The conversation ends and `flow:terminated` becomes true.",
                json!("terminate"),
            ),
        ],
        questions: QUESTION_RULES
            .iter()
            .map(|(name, meaning)| entry(name, meaning, Value::Null))
            .collect(),
        diagnostics: DIAGNOSTIC_CODES
            .iter()
            .map(|(name, meaning)| entry(name, meaning, Value::Null))
            .collect(),
    }
}

const QUESTION_RULES: [(&str, &str); 10] = [
    ("name", "The app has no name."),
    ("instruction", "The app has no base instruction."),
    (
        "tool_description:<tool>",
        "A declared tool has no description, so the model cannot tell when to call it.",
    ),
    (
        "commit_gate:<tool>",
        "A conversation allows a tool that looks like it changes something, and no stage \
         commits it behind a guard.",
    ),
    (
        "redact:<slot>",
        "A collected slot looks sensitive and no redact policy covers it.",
    ),
    ("voice", "An audio app has no voice."),
    (
        "greeting",
        "An audio app has no greeting, so it waits for the caller to speak first.",
    ),
    (
        "escalation",
        "The conversation has no way out: no digression, safety handoff or repair escalation.",
    ),
    (
        "disclosure",
        "An audio conversation reads no fixed text, such as a recording or AI disclosure.",
    ),
    (
        "tool_binding:<tool>",
        "A declared tool has no implementation: no HTTP or MCP binding and no canned response.",
    ),
];

const DIAGNOSTIC_CODES: [(&str, &str); 7] = [
    ("invalid_json", "The text is not JSON."),
    ("not_an_object", "The document is not a JSON object."),
    (
        "invalid_spec",
        "The document does not deserialize as a session spec.",
    ),
    (
        "unknown_field",
        "A field is not part of the spec and is ignored. Usually a misspelling.",
    ),
    (
        "unknown_tool",
        "The conversation references a tool that is not declared.",
    ),
    (
        "unwritten_key",
        "A guard reads a state key that nothing writes, so it can never become true.",
    ),
    (
        "validation",
        "A problem reported by spec validation, passed through as text.",
    ),
];

// ---------------------------------------------------------------------------
// Check
// ---------------------------------------------------------------------------

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The spec cannot run as written.
    Error,
    /// The spec runs but probably not as intended.
    Warning,
}

/// A mechanical repair for a diagnostic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Fix {
    /// What the patch does.
    pub description: String,
    /// The patch, computed against the checked document.
    pub patch: Vec<PatchOp>,
}

/// One problem found by [`check`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Diagnostic {
    /// Severity.
    pub severity: Severity,
    /// Stable code; see [`Catalog::diagnostics`].
    pub code: String,
    /// JSON pointer to the offending value, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// What is wrong.
    pub message: String,
    /// A mechanical repair, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

/// The result of [`check`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckReport {
    /// No error-severity diagnostics.
    pub valid: bool,
    /// Errors first, then warnings.
    pub diagnostics: Vec<Diagnostic>,
}

impl CheckReport {
    fn new(mut diagnostics: Vec<Diagnostic>) -> Self {
        diagnostics.sort_by_key(|d| d.severity != Severity::Error);
        let valid = diagnostics.iter().all(|d| d.severity != Severity::Error);
        Self { valid, diagnostics }
    }
}

fn diagnostic(
    severity: Severity,
    code: &str,
    path: Option<String>,
    message: impl Into<String>,
    fix: Option<Fix>,
) -> Diagnostic {
    Diagnostic {
        severity,
        code: code.into(),
        path,
        message: message.into(),
        fix,
    }
}

/// Validate `doc` and report structured diagnostics.
///
/// Conversation specs get pointer-level diagnostics for unknown tools and
/// unwritten guard keys, including digression triggers and safety-handoff
/// intents. Everything else that [`SessionSpec::validate`] reports is passed
/// through with code `validation`.
pub fn check(doc: &Value) -> CheckReport {
    let Some(object) = doc.as_object() else {
        return CheckReport::new(vec![diagnostic(
            Severity::Error,
            "not_an_object",
            Some(String::new()),
            "a spec must be a JSON object",
            None,
        )]);
    };
    let bare_flow = !object.contains_key("flow") && object.contains_key("steps");
    let mut diagnostics = Vec::new();
    if !bare_flow {
        unknown_fields(doc, &mut diagnostics);
    }
    let spec = match SessionSpec::from_value(doc.clone()) {
        Ok(spec) => spec,
        Err(message) => {
            diagnostics.push(diagnostic(
                Severity::Error,
                "invalid_spec",
                None,
                message,
                None,
            ));
            return CheckReport::new(diagnostics);
        }
    };

    // Messages the structured checks already report with a path.
    let mut covered = Vec::new();
    if spec.conversation.is_some() {
        unknown_tools(doc, &spec, &mut diagnostics, &mut covered);
        unwritten_keys(doc, &spec, &mut diagnostics, &mut covered);
    }
    let validation = spec.validate();
    let passes = |message: &String| !covered.iter().any(|c| message.contains(c.as_str()));
    for message in validation.errors.into_iter().filter(passes) {
        diagnostics.push(diagnostic(
            Severity::Error,
            "validation",
            None,
            message,
            None,
        ));
    }
    for message in validation.warnings.into_iter().filter(passes) {
        diagnostics.push(diagnostic(
            Severity::Warning,
            "validation",
            None,
            message,
            None,
        ));
    }
    CheckReport::new(diagnostics)
}

/// [`check`] the text of a spec file. Text that is not JSON gets an
/// `invalid_json` diagnostic with its line and column.
pub fn check_str(text: &str) -> CheckReport {
    match serde_json::from_str::<Value>(text) {
        Ok(doc) => check(&doc),
        Err(e) => CheckReport::new(vec![diagnostic(
            Severity::Error,
            "invalid_json",
            None,
            e.to_string(),
            None,
        )]),
    }
}

/// Field names of a definition in the spec's JSON Schema.
fn schema_fields(schema: &Value, definition: Option<&str>) -> BTreeSet<String> {
    let node = match definition {
        Some(name) => &schema["definitions"][name],
        None => schema,
    };
    node["properties"]
        .as_object()
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default()
}

fn unknown_fields(doc: &Value, out: &mut Vec<Diagnostic>) {
    let schema = SessionSpec::json_schema();
    let top = schema_fields(&schema, None);
    let mut stage = schema_fields(&schema, Some("StageSpec"));
    // `say` also deserializes from `instruction`.
    stage.insert("instruction".into());
    let conversation = schema_fields(&schema, Some("ConversationSpec"));
    let overlay = schema_fields(&schema, Some("OverlaySpec"));
    let tool = schema_fields(&schema, Some("ToolSpec"));

    unknown_in(doc, "", &top, out);
    for (i, t) in array(doc, "/tools") {
        unknown_in(t, &format!("/tools/{i}"), &tool, out);
    }
    let Some(conv) = doc.get("conversation").filter(|c| c.is_object()) else {
        return;
    };
    unknown_in(conv, "/conversation", &conversation, out);
    for (i, s) in array(doc, "/conversation/stages") {
        unknown_in(s, &format!("/conversation/stages/{i}"), &stage, out);
    }
    for (o, ov) in array(doc, "/conversation/overlays") {
        let base = format!("/conversation/overlays/{o}");
        unknown_in(ov, &base, &overlay, out);
        for (i, s) in array(ov, "/stages") {
            unknown_in(s, &format!("{base}/stages/{i}"), &stage, out);
        }
    }
}

fn array<'a>(doc: &'a Value, pointer: &str) -> impl Iterator<Item = (usize, &'a Value)> {
    doc.pointer(pointer)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
}

fn unknown_in(value: &Value, base: &str, known: &BTreeSet<String>, out: &mut Vec<Diagnostic>) {
    let Some(object) = value.as_object() else {
        return;
    };
    for (key, field) in object {
        // `$schema` and similar editor keys are allowed.
        if known.contains(key) || key.starts_with('$') {
            continue;
        }
        let path = format!("{base}/{}", escape(key));
        let suggestion = closest(key, known.iter().map(String::as_str));
        let message = match suggestion {
            Some(s) => format!("'{key}' is not a field here and is ignored — did you mean '{s}'?"),
            None => format!("'{key}' is not a field here and is ignored"),
        };
        let fix = suggestion
            .filter(|s| !object.contains_key(*s))
            .map(|s| Fix {
                description: format!("rename '{key}' to '{s}'"),
                patch: vec![
                    PatchOp::Remove { path: path.clone() },
                    add(format!("{base}/{}", escape(s)), field.clone()),
                ],
            });
        out.push(diagnostic(
            Severity::Warning,
            "unknown_field",
            Some(path),
            message,
            fix,
        ));
    }
}

/// The unique closest candidate within edit distance 2.
fn closest<'a>(word: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let mut best: Option<(usize, &str)> = None;
    let mut tied = false;
    for candidate in candidates {
        let distance = levenshtein(word, candidate);
        if distance == 0 || distance > 2 {
            continue;
        }
        match best {
            Some((d, _)) if distance > d => {}
            Some((d, _)) if distance == d => tied = true,
            _ => {
                best = Some((distance, candidate));
                tied = false;
            }
        }
    }
    best.filter(|_| !tied).map(|(_, c)| c)
}

/// A stage and the pointer to it, main flow first, then digressions.
fn stage_pointers(spec: &SessionSpec) -> Vec<(String, &StageSpec)> {
    let Some(conv) = &spec.conversation else {
        return Vec::new();
    };
    let main = conv
        .stages
        .iter()
        .enumerate()
        .map(|(i, s)| (format!("/conversation/stages/{i}"), s));
    let overlays = conv.overlays.iter().enumerate().flat_map(|(o, ov)| {
        ov.stages
            .iter()
            .enumerate()
            .map(move |(i, s)| (format!("/conversation/overlays/{o}/stages/{i}"), s))
    });
    main.chain(overlays).collect()
}

/// Where a guard sits, for describing what its keys mean.
#[derive(Debug, Clone)]
enum Site {
    Commit(String),
    Trigger,
    Other,
}

/// A guard atom that reads a name: its kind, the name, and the pointer to the
/// string holding it.
struct Atom {
    kind: &'static str,
    name: String,
    path: String,
}

fn guard_atoms(guard: &Value, path: &str, out: &mut Vec<Atom>) {
    let Some(object) = guard.as_object() else {
        return;
    };
    for (kind, inner) in object {
        let here = format!("{path}/{}", escape(kind));
        match kind.as_str() {
            "is_true" | "is_set" | "called_ok" | "done" => {
                if let Some(name) = inner.as_str() {
                    out.push(Atom {
                        kind: atom_kind(kind),
                        name: name.to_string(),
                        path: here,
                    });
                }
            }
            "eq" => {
                if let Some(name) = inner.get(0).and_then(Value::as_str) {
                    out.push(Atom {
                        kind: "eq",
                        name: name.to_string(),
                        path: format!("{here}/0"),
                    });
                }
            }
            "captured" => {
                for (i, name) in inner.as_array().into_iter().flatten().enumerate() {
                    if let Some(name) = name.as_str() {
                        out.push(Atom {
                            kind: "captured",
                            name: name.to_string(),
                            path: format!("{here}/{i}"),
                        });
                    }
                }
            }
            "all" | "any" => {
                for (i, g) in inner.as_array().into_iter().flatten().enumerate() {
                    guard_atoms(g, &format!("{here}/{i}"), out);
                }
            }
            "not" => guard_atoms(inner, &here, out),
            _ => {}
        }
    }
}

fn atom_kind(kind: &str) -> &'static str {
    match kind {
        "is_true" => "is_true",
        "is_set" => "is_set",
        "called_ok" => "called_ok",
        _ => "done",
    }
}

/// Every guard in the conversation, with its pointer and site.
fn conversation_guards(doc: &Value, spec: &SessionSpec) -> Vec<(String, Site)> {
    let mut guards = Vec::new();
    for (base, stage) in stage_pointers(spec) {
        if stage.done.is_some() {
            guards.push((format!("{base}/done"), Site::Other));
        }
        if let Some(commit) = &stage.commit {
            guards.push((
                format!("{base}/commit/when"),
                Site::Commit(commit.tool.clone()),
            ));
        }
        for j in 0..stage.next.len() {
            guards.push((format!("{base}/next/{j}/when"), Site::Other));
        }
    }
    for (o, _) in array(doc, "/conversation/overlays") {
        guards.push((format!("/conversation/overlays/{o}/trigger"), Site::Trigger));
    }
    guards
}

fn unknown_tools(
    doc: &Value,
    spec: &SessionSpec,
    out: &mut Vec<Diagnostic>,
    covered: &mut Vec<String>,
) {
    // MCP toolsets and skills resolve their tool names elsewhere.
    if !spec.mcp.is_empty() || !spec.skills.is_empty() {
        return;
    }
    let mut known: BTreeSet<&str> = spec.tools.iter().map(|t| t.name.as_str()).collect();
    if spec.memory.is_some() {
        known.extend(MEMORY_TOOL_NAMES);
    }
    // Validation only checks names against a non-empty registry.
    let severity = if spec.tools.is_empty() {
        Severity::Warning
    } else {
        Severity::Error
    };

    let mut refs: Vec<(String, String)> = Vec::new();
    for (base, stage) in stage_pointers(spec) {
        for (k, tool) in stage.allow.iter().enumerate() {
            refs.push((tool.clone(), format!("{base}/allow/{k}")));
        }
        if let Some(commit) = &stage.commit {
            refs.push((commit.tool.clone(), format!("{base}/commit/tool")));
        }
    }
    for (path, _) in conversation_guards(doc, spec) {
        let mut atoms = Vec::new();
        if let Some(guard) = doc.pointer(&path) {
            guard_atoms(guard, &path, &mut atoms);
        }
        refs.extend(
            atoms
                .into_iter()
                .filter(|a| a.kind == "called_ok")
                .map(|a| (a.name, a.path)),
        );
    }
    if let Some(conv) = &spec.conversation {
        for (p, policy) in conv.policies.iter().enumerate() {
            if let Policy::Commit {
                tool,
                compensate_with,
                ..
            } = policy
            {
                let base = format!("/conversation/policies/{p}");
                refs.push((tool.clone(), format!("{base}/tool")));
                if let Some(c) = compensate_with {
                    refs.push((c.clone(), format!("{base}/compensate_with")));
                }
            }
        }
    }

    for (name, path) in refs {
        if known.contains(name.as_str()) {
            continue;
        }
        covered.push(format!(
            "tool '{name}' which is not in the provided tool registry"
        ));
        let suggestion = closest(&name, known.iter().copied());
        let (message, fix) = match suggestion {
            Some(s) => (
                format!("tool '{name}' is not declared — did you mean '{s}'?"),
                Fix {
                    description: format!("replace '{name}' with '{s}'"),
                    patch: vec![PatchOp::Replace {
                        path: path.clone(),
                        value: json!(s),
                    }],
                },
            ),
            None => (
                format!("tool '{name}' is not declared"),
                Fix {
                    description: format!(
                        "declare '{name}' with no binding (codegen writes a stub for it)"
                    ),
                    patch: vec![append(doc, "/tools", json!({ "name": name }))],
                },
            ),
        };
        out.push(diagnostic(
            severity,
            "unknown_tool",
            Some(path),
            message,
            Some(fix),
        ));
    }
}

/// Every key the spec writes, plus the prefixes and suffixes the runtime
/// writes on its own.
fn is_written(key: &str, written: &BTreeSet<String>) -> bool {
    written.contains(key)
        || RUNTIME_WRITTEN.iter().any(|p| key.starts_with(p))
        || key.ends_with(":result")
}

/// One guard read of a state key.
struct KeyRead {
    kind: &'static str,
    path: String,
    /// A safety-handoff intent name: the key without its `intent:` prefix.
    intent_name: bool,
}

fn unwritten_keys(
    doc: &Value,
    spec: &SessionSpec,
    out: &mut Vec<Diagnostic>,
    covered: &mut Vec<String>,
) {
    let written = spec.state_keys_written();
    // Each key's most specific site (a commit or trigger beats any other
    // guard) and every read of it.
    let mut reads: BTreeMap<String, (Site, Vec<KeyRead>)> = BTreeMap::new();
    for (path, site) in conversation_guards(doc, spec) {
        let mut atoms = Vec::new();
        if let Some(guard) = doc.pointer(&path) {
            guard_atoms(guard, &path, &mut atoms);
        }
        for atom in atoms {
            if matches!(atom.kind, "called_ok" | "done") {
                continue;
            }
            let read = reads
                .entry(atom.name)
                .or_insert_with(|| (site.clone(), Vec::new()));
            if matches!(read.0, Site::Other) {
                read.0 = site.clone();
            }
            read.1.push(KeyRead {
                kind: atom.kind,
                path: atom.path,
                intent_name: false,
            });
        }
    }
    if let Some(conv) = &spec.conversation {
        for (p, policy) in conv.policies.iter().enumerate() {
            if let Policy::SafetyHandoff { intents } = policy {
                for (k, intent) in intents.iter().enumerate() {
                    let read = reads
                        .entry(format!("intent:{intent}"))
                        .or_insert_with(|| (Site::Trigger, Vec::new()));
                    read.0 = Site::Trigger;
                    read.1.push(KeyRead {
                        kind: "is_true",
                        path: format!("/conversation/policies/{p}/intents/{k}"),
                        intent_name: true,
                    });
                }
            }
        }
    }

    for (key, (site, reads)) in reads {
        if is_written(&key, &written) {
            continue;
        }
        covered.push(format!("state key '{key}'"));
        let path = reads[0].path.clone();
        // A handoff policy names the intent without its prefix, so only an
        // intent flag can replace it.
        let intent_only = reads.iter().any(|r| r.intent_name);
        let suggestion = closest(
            &key,
            written
                .iter()
                .map(String::as_str)
                .filter(|w| !intent_only || w.starts_with("intent:")),
        );
        let mut message = format!(
            "a guard reads state key '{key}' but nothing writes it, so it can never become true"
        );
        let fix = if let Some(s) = suggestion {
            message.push_str(&format!(" — did you mean '{s}'?"));
            let patch = reads
                .iter()
                .map(|r| {
                    let value = if r.intent_name {
                        s.strip_prefix("intent:").unwrap_or(s)
                    } else {
                        s
                    };
                    PatchOp::Replace {
                        path: r.path.clone(),
                        value: json!(value),
                    }
                })
                .collect();
            Some(Fix {
                description: format!("read '{s}' instead"),
                patch,
            })
        } else if reads.iter().all(|r| r.kind == "is_true") {
            Some(Fix {
                description: format!(
                    "add a '{SIGNALS_EXTRACTOR}' extractor field that sets '{key}' when the \
                     caller says so (needs an extraction model at run time)"
                ),
                patch: signal_ops(doc, &key, &signal_description(&key, &site)),
            })
        } else {
            None
        };
        out.push(diagnostic(
            Severity::Warning,
            "unwritten_key",
            Some(path),
            message,
            fix,
        ));
    }
}

fn humanize(key: &str) -> String {
    key.replace(['_', '-', ':'], " ")
}

fn signal_description(key: &str, site: &Site) -> String {
    match (site, key.strip_prefix("intent:")) {
        (_, Some(intent)) => format!("What the caller said matches the intent '{intent}'."),
        (Site::Commit(tool), None) => format!(
            "The caller explicitly agreed to go ahead with {} after hearing the details.",
            humanize(tool)
        ),
        _ => format!("The conversation shows that '{key}' is true."),
    }
}

/// The JSON field a signal extractor fills for `key`.
fn signal_field(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Ops that make the signal extractor write `key` as a boolean.
fn signal_ops(doc: &Value, key: &str, description: &str) -> Vec<PatchOp> {
    let field = signal_field(key);
    let promote = if field == key {
        json!({ "field": field, "policy": "true_only" })
    } else {
        json!({ "field": field, "to": key, "policy": "true_only" })
    };
    let property = json!({ "type": "boolean", "description": description });
    let existing = array(doc, "/extract").find(|(_, e)| e["name"] == SIGNALS_EXTRACTOR);
    match existing {
        Some((i, extractor)) => {
            let base = format!("/extract/{i}");
            let schema_op = if extractor["schema"]["properties"].is_object() {
                add(
                    format!("{base}/schema/properties/{}", escape(&field)),
                    property,
                )
            } else {
                add(
                    format!("{base}/schema"),
                    json!({ "type": "object", "properties": { field.clone(): property } }),
                )
            };
            vec![schema_op, append(doc, &format!("{base}/promote"), promote)]
        }
        None => vec![append(
            doc,
            "/extract",
            json!({
                "name": SIGNALS_EXTRACTOR,
                "instruction": "Read the latest turns of the conversation. Set a field to true \
                                only when the caller clearly said so in their own words. \
                                Otherwise leave it out.",
                "schema": { "type": "object", "properties": { field: property } },
                "promote": [promote]
            }),
        )],
    }
}

// ---------------------------------------------------------------------------
// Plan
// ---------------------------------------------------------------------------

/// Answered question ids, mapped to the chosen option value. A harness
/// stores this next to the spec and passes it back so answered questions
/// are not asked again.
pub type Decisions = BTreeMap<String, String>;

/// How a question is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// Pick one option.
    Choice,
    /// Supply text; the question has one option, which needs a value.
    Text,
}

/// One way to answer a question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AnswerOption {
    /// The value to send in [`Answer::choice`].
    pub value: String,
    /// What choosing it means.
    pub label: String,
    /// The patch that records it. Empty when the choice changes nothing in
    /// the spec and is only recorded in [`Decisions`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patch: Vec<PatchOp>,
    /// The option needs [`Answer::value`]; it replaces the
    /// [`ANSWER_PLACEHOLDER`] strings in `patch`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_value: bool,
}

/// A decision the document leaves open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Question {
    /// Stable id; see [`Catalog::questions`].
    pub id: String,
    /// The question, phrased for the user.
    pub ask: String,
    /// Why it matters.
    pub why: String,
    /// Choice or text.
    pub kind: QuestionKind,
    /// The ways to answer.
    pub options: Vec<AnswerOption>,
    /// The option to take when the user has no preference. Never one that
    /// needs a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The app should not be generated until this is answered.
    pub blocking: bool,
    /// Pointers the options write.
    pub affects: Vec<String>,
}

/// The result of [`plan`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Plan {
    /// No blocking questions and no error diagnostics: generate now.
    pub ready: bool,
    /// Number of blocking questions.
    pub blocking: usize,
    /// Open questions, blocking first.
    pub questions: Vec<Question>,
}

/// The open decisions in `doc`, skipping question ids in `decisions`.
///
/// Rules apply to full specs. A bare flow or a document that does not
/// deserialize has no questions; [`check`] reports why it does not parse.
pub fn plan(doc: &Value, decisions: &Decisions) -> Plan {
    let questions = questions(doc, decisions);
    let blocking = questions.iter().filter(|q| q.blocking).count();
    Plan {
        ready: blocking == 0 && check(doc).valid,
        blocking,
        questions,
    }
}

fn questions(doc: &Value, decisions: &Decisions) -> Vec<Question> {
    let bare_flow = doc.get("flow").is_none() && doc.get("steps").is_some();
    if bare_flow {
        return Vec::new();
    }
    let Ok(spec) = SessionSpec::from_value(doc.clone()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    ask_name_and_instruction(&spec, &mut out);
    ask_tool_descriptions(&spec, &mut out);
    ask_commit_gates(doc, &spec, &mut out);
    ask_redactions(doc, &spec, &mut out);
    ask_voice(&spec, &mut out);
    ask_greeting(&spec, &mut out);
    ask_escalation(doc, &spec, &mut out);
    ask_disclosure(&spec, &mut out);
    ask_tool_bindings(&spec, &mut out);
    out.retain(|q| !decisions.contains_key(&q.id));
    out.sort_by_key(|q| !q.blocking);
    out
}

fn text_question(id: String, ask: String, why: &str, path: String, blocking: bool) -> Question {
    Question {
        id,
        ask,
        why: why.into(),
        kind: QuestionKind::Text,
        options: vec![AnswerOption {
            value: "text".into(),
            label: "Your answer".into(),
            patch: vec![add(path.clone(), json!(ANSWER_PLACEHOLDER))],
            needs_value: true,
        }],
        default: None,
        blocking,
        affects: vec![path],
    }
}

fn option(value: &str, label: impl Into<String>, patch: Vec<PatchOp>) -> AnswerOption {
    AnswerOption {
        value: value.into(),
        label: label.into(),
        patch,
        needs_value: false,
    }
}

fn ask_name_and_instruction(spec: &SessionSpec, out: &mut Vec<Question>) {
    if spec.name.trim().is_empty() {
        out.push(text_question(
            "name".into(),
            "What is the app called? Use a short slug, such as `trattoria-booking`.".into(),
            "The name identifies the app in bundles, traces and generated projects.",
            "/name".into(),
            true,
        ));
    }
    if spec.instruction.trim().is_empty() {
        out.push(text_question(
            "instruction".into(),
            "What should the agent do, and for whom? One or two sentences.".into(),
            "The base instruction is the model's standing brief for every turn.",
            "/instruction".into(),
            true,
        ));
    }
}

fn ask_tool_descriptions(spec: &SessionSpec, out: &mut Vec<Question>) {
    for (i, tool) in spec.tools.iter().enumerate() {
        if tool.description.trim().is_empty() {
            out.push(text_question(
                format!("tool_description:{}", tool.name),
                format!("What does the tool `{}` do?", tool.name),
                "The model reads the description to decide when to call the tool.",
                format!("/tools/{i}/description"),
                true,
            ));
        }
    }
}

/// Verbs that suggest a tool changes something outside the conversation.
const COMMITTING_VERBS: [&str; 30] = [
    "add",
    "apply",
    "approve",
    "book",
    "buy",
    "cancel",
    "charge",
    "close",
    "create",
    "delete",
    "deposit",
    "enroll",
    "issue",
    "order",
    "pay",
    "place",
    "purchase",
    "refund",
    "register",
    "remove",
    "reschedule",
    "reserve",
    "schedule",
    "send",
    "sign",
    "submit",
    "transfer",
    "update",
    "withdraw",
    "write",
];

/// Lowercase words of a snake, kebab or camel case name.
fn name_parts(name: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut previous_lower = false;
    for c in name.chars() {
        if !c.is_ascii_alphanumeric() {
            parts.push(String::new());
            previous_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && previous_lower {
            parts.push(String::new());
        }
        previous_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        if let Some(part) = parts.last_mut() {
            part.push(c.to_ascii_lowercase());
        }
    }
    parts.retain(|p| !p.is_empty());
    parts
}

fn looks_committing(spec: &SessionSpec, tool: &str) -> bool {
    let verb = name_parts(tool).into_iter().next().unwrap_or_default();
    let mutating_http = spec
        .tools
        .iter()
        .find(|t| t.name == tool)
        .and_then(|t| t.http.as_ref())
        .is_some_and(|h| !h.method.eq_ignore_ascii_case("GET"));
    COMMITTING_VERBS.contains(&verb.as_str()) || mutating_http
}

fn ask_commit_gates(doc: &Value, spec: &SessionSpec, out: &mut Vec<Question>) {
    if spec.conversation.is_none() {
        return;
    }
    let stages = stage_pointers(spec);
    let committed: BTreeSet<&str> = stages
        .iter()
        .filter_map(|(_, s)| s.commit.as_ref().map(|c| c.tool.as_str()))
        .collect();
    let written = spec.state_keys_written();
    let mut seen = BTreeSet::new();
    for (_, stage) in &stages {
        for tool in &stage.allow {
            if committed.contains(tool.as_str())
                || !seen.insert(tool.as_str())
                || !looks_committing(spec, tool)
            {
                continue;
            }
            // The first stage allowing the tool that has no commit yet.
            let Some((base, _)) = stages
                .iter()
                .find(|(_, s)| s.commit.is_none() && s.allow.contains(tool))
            else {
                continue;
            };
            let key = format!("{tool}_confirmed");
            let mut patch = vec![add(
                format!("{base}/commit"),
                json!({ "tool": tool, "when": { "is_true": key } }),
            )];
            if !is_written(&key, &written) {
                patch.extend(signal_ops(
                    doc,
                    &key,
                    &signal_description(&key, &Site::Commit(tool.clone())),
                ));
            }
            out.push(Question {
                id: format!("commit_gate:{tool}"),
                ask: format!(
                    "Should `{tool}` run only after the caller confirms the details read back \
                     to them?"
                ),
                why: "A stage that allows a tool without a commit guard lets the model call it \
                      as soon as the stage is active, before the caller has agreed."
                    .into(),
                kind: QuestionKind::Choice,
                options: vec![
                    option(
                        "confirm",
                        format!(
                            "Gate it on '{key}', which an extractor sets when the caller \
                             agrees"
                        ),
                        patch,
                    ),
                    option(
                        "no_confirmation",
                        "It changes nothing the caller has to approve",
                        Vec::new(),
                    ),
                ],
                default: Some("confirm".into()),
                blocking: true,
                affects: vec![format!("{base}/commit"), "/extract".into()],
            });
        }
    }
}

/// Name parts that suggest a slot holds personal or payment data.
const SENSITIVE_PARTS: [&str; 19] = [
    "account",
    "birth",
    "card",
    "cvc",
    "cvv",
    "diagnosis",
    "dob",
    "iban",
    "insurance",
    "licence",
    "license",
    "medical",
    "passcode",
    "passport",
    "password",
    "pin",
    "routing",
    "ssn",
    "tax",
];

fn looks_sensitive(slot: &str) -> bool {
    name_parts(slot)
        .iter()
        .any(|part| SENSITIVE_PARTS.contains(&part.as_str()))
}

fn ask_redactions(doc: &Value, spec: &SessionSpec, out: &mut Vec<Question>) {
    let Some(conv) = &spec.conversation else {
        return;
    };
    let mut redacted = BTreeSet::new();
    let mut redact_policy = None;
    for (p, policy) in conv.policies.iter().enumerate() {
        if let Policy::Redact { keys } = policy {
            redacted.extend(keys.iter().map(String::as_str));
            redact_policy.get_or_insert(p);
        }
    }
    let mut slots = BTreeSet::new();
    for (_, stage) in stage_pointers(spec) {
        slots.extend(stage.collect.iter().map(String::as_str));
        slots.extend(stage.resolve.iter().map(|r| r.slot.as_str()));
    }
    for slot in slots {
        if redacted.contains(slot) || !looks_sensitive(slot) {
            continue;
        }
        let patch = match redact_policy {
            Some(p) => vec![add(
                format!("/conversation/policies/{p}/keys/-"),
                json!(slot),
            )],
            None => vec![append(
                doc,
                "/conversation/policies",
                json!({ "kind": "redact", "keys": [slot] }),
            )],
        };
        out.push(Question {
            id: format!("redact:{slot}"),
            ask: format!("`{slot}` looks sensitive. Should it be redacted?"),
            why: "A redacted key is written as `[redacted]` to the journal, persistence \
                  snapshots and extraction events. In-process reads still see the value, and \
                  transcript text is not covered: see the hardening guide."
                .into(),
            kind: QuestionKind::Choice,
            options: vec![
                option("redact", "Redact it", patch),
                option(
                    "keep",
                    "Keep it in the clear (it is not sensitive, or a later system needs it)",
                    Vec::new(),
                ),
            ],
            default: Some("redact".into()),
            blocking: true,
            affects: vec!["/conversation/policies".into()],
        });
    }
}

fn ask_voice(spec: &SessionSpec, out: &mut Vec<Question>) {
    if spec.modality != super::SpecModality::Audio || spec.voice.is_some() {
        return;
    }
    out.push(Question {
        id: "voice".into(),
        ask: "Which voice should the agent speak with?".into(),
        why: "Without one the provider default (Puck) is used.".into(),
        kind: QuestionKind::Choice,
        options: VOICES
            .iter()
            .map(|v| option(v, *v, vec![add("/voice", json!(v))]))
            .collect(),
        default: Some("Puck".into()),
        blocking: false,
        affects: vec!["/voice".into()],
    });
}

fn ask_greeting(spec: &SessionSpec, out: &mut Vec<Question>) {
    if spec.modality != super::SpecModality::Audio || spec.greeting.is_some() {
        return;
    }
    let greeting = if spec.name.trim().is_empty() {
        "Greet the caller and ask how you can help.".to_string()
    } else {
        format!(
            "Greet the caller as the assistant for {} and ask how you can help.",
            spec.name
        )
    };
    out.push(Question {
        id: "greeting".into(),
        ask: "Should the agent speak first when the call connects?".into(),
        why: "On a phone call the caller usually expects to hear the agent first. Without a \
              greeting the agent waits for the caller."
            .into(),
        kind: QuestionKind::Choice,
        options: vec![
            option(
                "speak_first",
                format!("Speak first: \"{greeting}\""),
                vec![add("/greeting", json!(greeting))],
            ),
            AnswerOption {
                value: "custom".into(),
                label: "Speak first, with your own greeting instruction".into(),
                patch: vec![add("/greeting", json!(ANSWER_PLACEHOLDER))],
                needs_value: true,
            },
            option("wait", "Wait for the caller to speak", Vec::new()),
        ],
        default: Some("speak_first".into()),
        blocking: false,
        affects: vec!["/greeting".into()],
    });
}

fn ask_escalation(doc: &Value, spec: &SessionSpec, out: &mut Vec<Question>) {
    let Some(conv) = &spec.conversation else {
        return;
    };
    let handoff = conv
        .policies
        .iter()
        .any(|p| matches!(p, Policy::SafetyHandoff { .. }));
    let escalates = conv
        .stages
        .iter()
        .any(|s| s.repair.as_ref().is_some_and(|r| r.escalate_to.is_some()));
    if handoff || escalates || !conv.overlays.is_empty() {
        return;
    }
    // A digression whose stage admits only the transfer tool and waits for
    // it. A terminal digression stage, which `safety_handoff` lowers to,
    // restricts no tools on the turn it is entered.
    let key = "intent:human_agent";
    let transfer = "handoff_to_staff";
    let mut patch = Vec::new();
    if !spec.tools.iter().any(|t| t.name == transfer) {
        patch.push(append(
            doc,
            "/tools",
            json!({
                "name": transfer,
                "description": "Transfer the caller to a member of staff."
            }),
        ));
    }
    patch.push(append(
        doc,
        "/conversation/overlays",
        json!({
            "name": "handoff",
            "trigger": { "is_true": key },
            "stages": [{
                "id": "transfer",
                "say": "Tell the caller you are passing them to a member of staff, then call handoff_to_staff.",
                "allow": [transfer],
                "done": { "called_ok": transfer }
            }],
            "resume": "terminate"
        }),
    ));
    if !spec.state_keys_written().contains(key) {
        patch.extend(signal_ops(
            doc,
            key,
            "The caller asked to speak to a person instead of the assistant.",
        ));
    }
    out.push(Question {
        id: "escalation".into(),
        ask: "What should happen when the caller asks for a person?".into(),
        why: "The conversation has no digression, safety handoff or repair escalation, so a \
              caller who wants out has no path."
            .into(),
        kind: QuestionKind::Choice,
        options: vec![
            option(
                "handoff",
                "Hand off through a `handoff_to_staff` tool your app implements; the \
                 conversation then ends and `flow:terminated` becomes true",
                patch,
            ),
            option("none", "Keep the agent in the conversation", Vec::new()),
        ],
        default: Some("handoff".into()),
        blocking: false,
        affects: vec![
            "/tools".into(),
            "/conversation/overlays".into(),
            "/extract".into(),
        ],
    });
}

fn ask_disclosure(spec: &SessionSpec, out: &mut Vec<Question>) {
    let Some(conv) = &spec.conversation else {
        return;
    };
    if spec.modality != super::SpecModality::Audio
        || conv.stages.is_empty()
        || conv.stages.iter().any(|s| s.verbatim.is_some())
    {
        return;
    }
    let path = "/conversation/stages/0/verbatim".to_string();
    out.push(Question {
        id: "disclosure".into(),
        ask: format!(
            "Should the agent read a fixed disclosure in the `{}` stage, such as a \
             recording or AI notice?",
            conv.stages[0].id
        ),
        why: "Text that must be said word for word belongs in a verbatim stage, which does \
              not complete until it has been said."
            .into(),
        kind: QuestionKind::Choice,
        options: vec![
            AnswerOption {
                value: "read".into(),
                label: "Read this text word for word".into(),
                patch: vec![add(path.clone(), json!(ANSWER_PLACEHOLDER))],
                needs_value: true,
            },
            option("none", "No fixed text", Vec::new()),
        ],
        default: Some("none".into()),
        blocking: false,
        affects: vec![path],
    });
}

fn ask_tool_bindings(spec: &SessionSpec, out: &mut Vec<Question>) {
    for (i, tool) in spec.tools.iter().enumerate() {
        if tool.http.is_some() || tool.mcp.is_some() || tool.response.is_some() {
            continue;
        }
        let method = if looks_committing(spec, &tool.name) {
            "POST"
        } else {
            "GET"
        };
        out.push(Question {
            id: format!("tool_binding:{}", tool.name),
            ask: format!("How is `{}` implemented?", tool.name),
            why: "A tool with no binding returns `{\"ok\": true}` until it is implemented.".into(),
            kind: QuestionKind::Choice,
            options: vec![
                option(
                    "stub",
                    "In the generated project: codegen writes a typed stub to fill in",
                    Vec::new(),
                ),
                AnswerOption {
                    value: "http".into(),
                    label: format!("An HTTP endpoint ({method}); give its URL"),
                    patch: vec![add(
                        format!("/tools/{i}/http"),
                        json!({ "method": method, "url": ANSWER_PLACEHOLDER }),
                    )],
                    needs_value: true,
                },
                AnswerOption {
                    value: "mcp".into(),
                    label: "An MCP server; give its command line or http(s) URL".into(),
                    patch: vec![add(format!("/tools/{i}/mcp"), json!(ANSWER_PLACEHOLDER))],
                    needs_value: true,
                },
            ],
            default: Some("stub".into()),
            blocking: false,
            affects: vec![format!("/tools/{i}")],
        });
    }
}

// ---------------------------------------------------------------------------
// Answer
// ---------------------------------------------------------------------------

/// One answer to a [`Question`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Answer {
    /// The question id.
    pub id: String,
    /// The chosen option. Omitted: the only option of a text question, or
    /// the question's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    /// The value for an option that needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

impl Answer {
    /// Choose an option.
    pub fn choose(id: impl Into<String>, choice: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            choice: Some(choice.into()),
            value: None,
        }
    }

    /// Answer a text question.
    pub fn with_value(id: impl Into<String>, value: Value) -> Self {
        Self {
            id: id.into(),
            choice: None,
            value: Some(value),
        }
    }
}

/// The result of [`answer`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Answered {
    /// The updated document.
    pub spec: Value,
    /// Question ids applied, in order.
    pub applied: Vec<String>,
    /// The input decisions plus the new ones.
    pub decisions: Decisions,
}

/// Why answers could not be applied. Nothing is applied when any answer
/// fails.
#[derive(Debug, Clone, PartialEq)]
pub enum AnswerError {
    /// The question is not open: answered already, decided, or no longer
    /// applies after earlier answers.
    NotOpen(String),
    /// No choice was given and the question has no default.
    MissingChoice(String),
    /// The choice is not one of the question's options.
    UnknownChoice {
        /// Question id.
        id: String,
        /// The choice given.
        choice: String,
        /// The valid option values.
        options: Vec<String>,
    },
    /// The option needs a non-empty value.
    MissingValue {
        /// Question id.
        id: String,
        /// The option chosen.
        choice: String,
    },
    /// The option's patch did not apply.
    Patch {
        /// Question id.
        id: String,
        /// The patch failure.
        error: PatchError,
    },
    /// The answer left a document that no longer deserializes.
    Invalid {
        /// Question id.
        id: String,
        /// The deserialization error.
        message: String,
    },
}

impl std::fmt::Display for AnswerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnswerError::NotOpen(id) => write!(
                f,
                "question '{id}' is not open: no such question, already decided, or it no \
                 longer applies"
            ),
            AnswerError::MissingChoice(id) => {
                write!(f, "question '{id}' needs a choice: it has no default")
            }
            AnswerError::UnknownChoice {
                id,
                choice,
                options,
            } => write!(
                f,
                "question '{id}' has no option '{choice}' (options: {})",
                options.join(", ")
            ),
            AnswerError::MissingValue { id, choice } => {
                write!(f, "question '{id}', option '{choice}' needs a value")
            }
            AnswerError::Patch { id, error } => write!(f, "question '{id}': {error}"),
            AnswerError::Invalid { id, message } => {
                write!(f, "question '{id}' left an invalid spec: {message}")
            }
        }
    }
}

impl std::error::Error for AnswerError {}

/// Apply `answers` in order. Before each one the document is planned again,
/// so a patch never comes from a stale plan.
pub fn answer(
    doc: &Value,
    answers: &[Answer],
    decisions: &Decisions,
) -> Result<Answered, AnswerError> {
    let mut spec = doc.clone();
    let mut decisions = decisions.clone();
    let mut applied = Vec::new();
    for a in answers {
        let question = questions(&spec, &decisions)
            .into_iter()
            .find(|q| q.id == a.id)
            .ok_or_else(|| AnswerError::NotOpen(a.id.clone()))?;
        let choice = match (&a.choice, question.options.as_slice()) {
            (Some(choice), _) => choice.clone(),
            (None, [only]) => only.value.clone(),
            (None, _) => question
                .default
                .clone()
                .ok_or_else(|| AnswerError::MissingChoice(a.id.clone()))?,
        };
        let chosen = question
            .options
            .iter()
            .find(|o| o.value == choice)
            .ok_or_else(|| AnswerError::UnknownChoice {
                id: a.id.clone(),
                choice: choice.clone(),
                options: question.options.iter().map(|o| o.value.clone()).collect(),
            })?;
        let patch = if chosen.needs_value {
            let value = a
                .value
                .clone()
                .filter(|v| v.as_str().is_none_or(|s| !s.trim().is_empty()) && !v.is_null())
                .ok_or_else(|| AnswerError::MissingValue {
                    id: a.id.clone(),
                    choice: choice.clone(),
                })?;
            chosen
                .patch
                .iter()
                .map(|op| substitute(op, &value))
                .collect()
        } else {
            chosen.patch.clone()
        };
        spec = apply_patch(&spec, &patch).map_err(|error| AnswerError::Patch {
            id: a.id.clone(),
            error,
        })?;
        SessionSpec::from_value(spec.clone()).map_err(|message| AnswerError::Invalid {
            id: a.id.clone(),
            message,
        })?;
        decisions.insert(question.id.clone(), choice);
        applied.push(question.id);
    }
    Ok(Answered {
        spec,
        applied,
        decisions,
    })
}

fn substitute(op: &PatchOp, answer: &Value) -> PatchOp {
    fn fill(value: &Value, answer: &Value) -> Value {
        match value {
            Value::String(s) if s == ANSWER_PLACEHOLDER => answer.clone(),
            Value::Array(items) => Value::Array(items.iter().map(|v| fill(v, answer)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), fill(v, answer)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    match op {
        PatchOp::Add { path, value } => PatchOp::Add {
            path: path.clone(),
            value: fill(value, answer),
        },
        PatchOp::Replace { path, value } => PatchOp::Replace {
            path: path.clone(),
            value: fill(value, answer),
        },
        PatchOp::Remove { path } => PatchOp::Remove { path: path.clone() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn booking() -> Value {
        json!({
            "name": "trattoria",
            "instruction": "Book tables for Trattoria Rustica.",
            "modality": "audio",
            "voice": "Charon",
            "greeting": "Welcome the caller.",
            "tools": [
                { "name": "check_availability", "description": "Check a time.", "response": { "available": true } },
                { "name": "book_table", "description": "Book the table.", "response": { "booked": true } }
            ],
            "conversation": {
                "name": "booking",
                "stages": [
                    {
                        "id": "collect",
                        "say": "Find out the party size and time.",
                        "verbatim": "This call may be recorded.",
                        "collect": ["party_size", "slot"],
                        "allow": ["check_availability"],
                        "next": [{ "to": "confirm", "when": { "captured": ["party_size", "slot"] } }]
                    },
                    {
                        "id": "confirm",
                        "say": "Read the booking back and book it once the caller agrees.",
                        "allow": ["book_table"],
                        "next": [{ "to": "done", "when": { "called_ok": "book_table" } }]
                    },
                    { "id": "done", "terminal": true }
                ],
                "require": ["done"],
                "policies": [{ "kind": "safety_handoff", "intents": ["human_agent"] }]
            },
            "extract": [{
                "name": "caller_signals",
                "instruction": "Signals.",
                "schema": { "type": "object", "properties": { "intent_human_agent": { "type": "boolean" } } },
                "promote": [{ "field": "intent_human_agent", "to": "intent:human_agent", "policy": "true_only" }]
            }]
        })
    }

    fn codes(report: &CheckReport) -> Vec<&str> {
        report.diagnostics.iter().map(|d| d.code.as_str()).collect()
    }

    fn fix(report: &CheckReport, code: &str) -> Vec<PatchOp> {
        report
            .diagnostics
            .iter()
            .find(|d| d.code == code)
            .and_then(|d| d.fix.clone())
            .unwrap_or_else(|| panic!("no fix for {code}: {report:#?}"))
            .patch
    }

    #[test]
    fn patch_add_replace_remove() {
        let doc = json!({ "a": [1, 3], "b": { "c": 1 } });
        let out = apply_patch(
            &doc,
            &[
                add("/a/1", json!(2)),
                add("/a/-", json!(4)),
                PatchOp::Replace {
                    path: "/b/c".into(),
                    value: json!(2),
                },
                add("/b/d~1e", json!(true)),
                PatchOp::Remove {
                    path: "/a/0".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(out, json!({ "a": [2, 3, 4], "b": { "c": 2, "d/e": true } }));
    }

    #[test]
    fn patch_is_atomic_and_reports_the_failing_op() {
        let doc = json!({ "a": 1 });
        let err = apply_patch(
            &doc,
            &[
                add("/b", json!(2)),
                PatchOp::Replace {
                    path: "/missing".into(),
                    value: json!(0),
                },
            ],
        )
        .unwrap_err();
        assert_eq!(err.index, 1);
        assert_eq!(err.path, "/missing");
        assert!(apply_patch(&doc, &[add("/x/y", json!(1))]).is_err());
        assert!(apply_patch(&json!([1]), &[add("/01", json!(1))]).is_err());
        assert!(apply_patch(&json!([1]), &[add("/2", json!(1))]).is_err());
        assert!(apply_patch(&doc, &[PatchOp::Remove { path: "".into() }]).is_err());
    }

    #[test]
    fn patch_ops_use_rfc6902_json() {
        let op: PatchOp =
            serde_json::from_value(json!({ "op": "add", "path": "/a", "value": 1 })).unwrap();
        assert_eq!(op, add("/a", json!(1)));
        assert_eq!(
            serde_json::to_value(PatchOp::Remove { path: "/a".into() }).unwrap(),
            json!({ "op": "remove", "path": "/a" })
        );
    }

    #[test]
    fn catalog_examples_deserialize() {
        let catalog = catalog();
        for g in &catalog.guards {
            serde_json::from_value::<gemini_adk_rs::flow::Guard>(g.example.clone())
                .unwrap_or_else(|e| panic!("guard {}: {e}", g.name));
        }
        for p in &catalog.policies {
            let policy: Policy = serde_json::from_value(p.example.clone())
                .unwrap_or_else(|e| panic!("policy {}: {e}", p.name));
            assert_eq!(serde_json::to_value(&policy).unwrap()["kind"], p.name);
        }
        for b in &catalog.tool_bindings {
            serde_json::from_value::<super::super::ToolSpec>(b.example.clone())
                .unwrap_or_else(|e| panic!("binding {}: {e}", b.name));
        }
        for r in &catalog.resume {
            serde_json::from_value::<crate::conversation::Resume>(r.example.clone())
                .unwrap_or_else(|e| panic!("resume {}: {e}", r.name));
        }
        for m in &catalog.modalities {
            serde_json::from_value::<super::super::SpecModality>(json!(m)).unwrap();
        }
        for v in &catalog.voices {
            let voice: gemini_genai_rs::prelude::Voice = serde_json::from_value(json!(v)).unwrap();
            assert_eq!(&voice.to_string(), v);
            assert!(!matches!(voice, gemini_genai_rs::prelude::Voice::Custom(_)));
        }
    }

    #[test]
    fn catalog_lists_every_question_rule() {
        // Every question id the rules can produce has a catalog entry.
        let doc = json!({
            "modality": "audio",
            "tools": [{ "name": "book_table" }],
            "conversation": {
                "name": "c",
                "stages": [
                    { "id": "collect", "say": "s", "collect": ["card_number"], "allow": ["book_table"],
                      "next": [{ "to": "done", "when": { "called_ok": "book_table" } }] },
                    { "id": "done", "terminal": true }
                ]
            }
        });
        let ids: BTreeSet<String> = plan(&doc, &Decisions::new())
            .questions
            .into_iter()
            .map(|q| q.id.split(':').next().unwrap().to_string())
            .collect();
        let catalog: BTreeSet<String> = catalog()
            .questions
            .into_iter()
            .map(|q| q.name.split(':').next().unwrap().to_string())
            .collect();
        assert_eq!(ids, catalog);
    }

    #[test]
    fn a_complete_spec_checks_clean_and_has_one_question() {
        let doc = booking();
        let report = check(&doc);
        assert!(report.valid, "{report:#?}");
        assert!(report.diagnostics.is_empty(), "{report:#?}");
        let plan = plan(&doc, &Decisions::new());
        let ids: Vec<&str> = plan.questions.iter().map(|q| q.id.as_str()).collect();
        assert_eq!(ids, ["commit_gate:book_table"]);
        assert!(!plan.ready);
    }

    #[test]
    fn misspelled_fields_are_reported_with_a_rename() {
        let mut doc = booking();
        let instruction = doc["instruction"].take();
        doc.as_object_mut().unwrap().remove("instruction");
        doc["instructions"] = instruction;
        doc["conversation"]["stages"][0]["colect"] = json!(["x"]);
        doc["$schema"] = json!("ignored");
        let report = check(&doc);
        let unknown: Vec<_> = report
            .diagnostics
            .iter()
            .filter(|d| d.code == "unknown_field")
            .map(|d| d.path.clone().unwrap())
            .collect();
        assert_eq!(unknown, ["/instructions", "/conversation/stages/0/colect"]);
        let fixed = apply_patch(&doc, &fix(&report, "unknown_field")).unwrap();
        assert_eq!(fixed["instruction"], "Book tables for Trattoria Rustica.");
        assert!(fixed.get("instructions").is_none());
    }

    #[test]
    fn unknown_tools_get_a_path_and_a_fix() {
        let mut doc = booking();
        doc["conversation"]["stages"][1]["allow"] = json!(["book_tabel"]);
        let report = check(&doc);
        assert!(!report.valid);
        let d = &report.diagnostics[0];
        assert_eq!(d.code, "unknown_tool");
        assert_eq!(d.path.as_deref(), Some("/conversation/stages/1/allow/0"));
        // The validator's own message is not repeated.
        assert!(!codes(&report).contains(&"validation"), "{report:#?}");
        let fixed = apply_patch(&doc, &fix(&report, "unknown_tool")).unwrap();
        assert!(check(&fixed).valid);

        // No close match: declare it.
        let mut doc = booking();
        doc["conversation"]["stages"][1]["allow"] = json!(["refund"]);
        let report = check(&doc);
        let fixed = apply_patch(&doc, &fix(&report, "unknown_tool")).unwrap();
        assert_eq!(fixed["tools"][2], json!({ "name": "refund" }));
        assert!(check(&fixed).valid, "{:#?}", check(&fixed));
    }

    #[test]
    fn unwritten_commit_key_gets_a_signal_extractor() {
        let mut doc = booking();
        doc["conversation"]["stages"][1]["commit"] =
            json!({ "tool": "book_table", "when": { "is_true": "user_confirmed" } });
        let report = check(&doc);
        assert_eq!(codes(&report), ["unwritten_key"], "{report:#?}");
        assert_eq!(
            report.diagnostics[0].path.as_deref(),
            Some("/conversation/stages/1/commit/when/is_true")
        );
        let fixed = apply_patch(&doc, &fix(&report, "unwritten_key")).unwrap();
        let signals = &fixed["extract"][0];
        assert_eq!(
            signals["schema"]["properties"]["user_confirmed"]["type"],
            "boolean"
        );
        assert_eq!(
            signals["promote"][1],
            json!({ "field": "user_confirmed", "policy": "true_only" })
        );
        assert!(check(&fixed).diagnostics.is_empty());
    }

    #[test]
    fn unwritten_key_prefers_a_close_written_key() {
        let mut doc = booking();
        doc["conversation"]["stages"][0]["next"][0]["when"] =
            json!({ "all": [{ "captured": ["party_sise"] }, { "is_set": "slot" }] });
        let report = check(&doc);
        let d = report
            .diagnostics
            .iter()
            .find(|d| d.code == "unwritten_key")
            .unwrap();
        assert_eq!(
            d.path.as_deref(),
            Some("/conversation/stages/0/next/0/when/all/0/captured/0")
        );
        let fixed = apply_patch(&doc, &fix(&report, "unwritten_key")).unwrap();
        assert!(check(&fixed).diagnostics.is_empty());
    }

    #[test]
    fn digression_triggers_and_handoff_intents_need_a_writer() {
        // Validation does not see digression triggers; check does.
        let mut doc = booking();
        doc.as_object_mut().unwrap().remove("extract");
        doc["conversation"]["overlays"] = json!([{
            "name": "cancel",
            "trigger": { "is_true": "intent:cancel" },
            "stages": [{ "id": "bye", "say": "Say goodbye.", "terminal": true }],
            "resume": "terminate"
        }]);
        let report = check(&doc);
        let paths: Vec<_> = report
            .diagnostics
            .iter()
            .filter(|d| d.code == "unwritten_key")
            .map(|d| d.path.clone().unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                "/conversation/overlays/0/trigger/is_true",
                "/conversation/policies/0/intents/0"
            ]
        );
        // One fix at a time, checking again in between.
        for _ in 0..2 {
            let report = check(&doc);
            doc = apply_patch(&doc, &fix(&report, "unwritten_key")).unwrap();
        }
        assert!(check(&doc).diagnostics.is_empty(), "{:#?}", check(&doc));
        let promote = &doc["extract"][0]["promote"];
        assert_eq!(promote[0]["to"], "intent:cancel");
        assert_eq!(promote[1]["to"], "intent:human_agent");
    }

    #[test]
    fn invalid_documents_are_reported_not_planned() {
        let report = check_str("{\"name\": ");
        assert_eq!(codes(&report), ["invalid_json"]);
        assert!(report.diagnostics[0].message.contains("line 1"));
        assert_eq!(codes(&check(&json!([]))), ["not_an_object"]);
        let doc = json!({ "name": 3 });
        assert_eq!(codes(&check(&doc)), ["invalid_spec"]);
        let plan = plan(&doc, &Decisions::new());
        assert!(plan.questions.is_empty() && !plan.ready);
    }

    #[test]
    fn validation_messages_pass_through() {
        let mut doc = booking();
        doc["conversation"]["stages"][1]["commit"] =
            json!({ "tool": "book_table", "when": "always" });
        let report = check(&doc);
        assert!(!report.valid);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "validation" && d.message.contains("always-true"))
        );
    }

    #[test]
    fn plan_asks_the_open_questions_blocking_first() {
        let doc = json!({
            "modality": "audio",
            "tools": [{ "name": "book_table" }],
            "conversation": {
                "name": "booking",
                "stages": [
                    { "id": "collect", "say": "s", "collect": ["party_size", "card_number"],
                      "next": [{ "to": "confirm", "when": { "captured": ["party_size"] } }] },
                    { "id": "confirm", "say": "c", "allow": ["book_table"],
                      "next": [{ "to": "done", "when": { "called_ok": "book_table" } }] },
                    { "id": "done", "terminal": true }
                ]
            }
        });
        let plan = plan(&doc, &Decisions::new());
        let ids: Vec<&str> = plan.questions.iter().map(|q| q.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "name",
                "instruction",
                "tool_description:book_table",
                "commit_gate:book_table",
                "redact:card_number",
                "voice",
                "greeting",
                "escalation",
                "disclosure",
                "tool_binding:book_table",
            ]
        );
        assert_eq!(plan.blocking, 5);
        // A committing tool is bound with POST.
        let binding = plan.questions.last().unwrap();
        let http = binding.options.iter().find(|o| o.value == "http").unwrap();
        assert_eq!(
            http.patch,
            [add(
                "/tools/0/http",
                json!({ "method": "POST", "url": ANSWER_PLACEHOLDER })
            )]
        );
        assert!(!plan.ready);
        for q in &plan.questions {
            if let Some(default) = &q.default {
                let option = q.options.iter().find(|o| &o.value == default).unwrap();
                assert!(!option.needs_value, "{} default needs a value", q.id);
            }
        }
    }

    #[test]
    fn answering_everything_makes_the_spec_ready() {
        let doc = json!({
            "modality": "audio",
            "tools": [{ "name": "book_table" }],
            "conversation": {
                "name": "booking",
                "stages": [
                    { "id": "collect", "say": "s", "collect": ["party_size", "card_number"],
                      "next": [{ "to": "confirm", "when": { "captured": ["party_size"] } }] },
                    { "id": "confirm", "say": "c", "allow": ["book_table"],
                      "next": [{ "to": "done", "when": { "called_ok": "book_table" } }] },
                    { "id": "done", "terminal": true }
                ],
                "require": ["done"]
            }
        });
        let answers = [
            Answer::with_value("name", json!("trattoria")),
            Answer::with_value("instruction", json!("Book tables.")),
            Answer::with_value("tool_description:book_table", json!("Book a table.")),
            Answer::choose("commit_gate:book_table", "confirm"),
            Answer::choose("redact:card_number", "redact"),
            Answer::choose("voice", "Kore"),
            Answer {
                id: "greeting".into(),
                choice: None,
                value: None,
            },
            Answer::choose("escalation", "handoff"),
            Answer {
                id: "disclosure".into(),
                choice: Some("read".into()),
                value: Some(json!("This call is with an automated assistant.")),
            },
            Answer {
                id: "tool_binding:book_table".into(),
                choice: Some("mcp".into()),
                value: Some(json!("python tools/server.py")),
            },
        ];
        let mut answers = answers.to_vec();
        // Answering `escalation` declares the transfer tool, which then has
        // its own binding question.
        answers.push(Answer::choose("tool_binding:handoff_to_staff", "stub"));
        let out = answer(&doc, &answers, &Decisions::new()).unwrap();
        assert_eq!(out.applied.len(), answers.len());
        assert_eq!(out.decisions["greeting"], "speak_first");
        let spec = &out.spec;
        assert_eq!(spec["voice"], "Kore");
        assert_eq!(
            spec["conversation"]["stages"][1]["commit"],
            json!({ "tool": "book_table", "when": { "is_true": "book_table_confirmed" } })
        );
        assert_eq!(
            spec["conversation"]["policies"],
            json!([{ "kind": "redact", "keys": ["card_number"] }])
        );
        assert_eq!(spec["conversation"]["overlays"][0]["name"], "handoff");
        assert_eq!(spec["tools"][1]["name"], "handoff_to_staff");
        // Both signals share one extractor.
        assert_eq!(spec["extract"].as_array().unwrap().len(), 1);
        assert_eq!(spec["extract"][0]["promote"].as_array().unwrap().len(), 2);
        assert_eq!(spec["tools"][0]["mcp"], "python tools/server.py");

        let report = check(spec);
        assert!(report.diagnostics.is_empty(), "{report:#?}");
        let plan = plan(spec, &out.decisions);
        assert!(plan.questions.is_empty() && plan.ready, "{plan:#?}");
    }

    #[tokio::test]
    async fn the_generated_handoff_admits_only_the_transfer() {
        // A terminal digression stage restricts nothing on the turn it is
        // entered; the generated one admits only the transfer until it runs.
        let doc = booking();
        let mut doc = doc;
        doc["conversation"]
            .as_object_mut()
            .unwrap()
            .remove("policies");
        doc.as_object_mut().unwrap().remove("extract");
        let out = answer(
            &doc,
            &[Answer::choose("escalation", "handoff")],
            &Decisions::new(),
        )
        .unwrap();
        let mut spec = out.spec;
        spec["scenarios"] = json!([{
            "name": "asking for a person admits only the transfer",
            "steps": [
                { "set": { "key": "intent:human_agent", "value": true } },
                "turn",
                { "expect_denied": "book_table" },
                { "expect_denied": "check_availability" },
                { "expect_allowed": "handoff_to_staff" },
                { "tool_ok": "handoff_to_staff" },
                "turn",
                { "expect_denied": "handoff_to_staff" },
                { "expect_denied": "book_table" }
            ]
        }]);
        assert!(check(&spec).valid, "{:#?}", check(&spec));
        let reports = SessionSpec::from_value(spec).unwrap().run_scenarios().await;
        assert!(reports.iter().all(|r| r.passed), "{reports:#?}");
    }

    #[test]
    fn decisions_suppress_questions_that_leave_the_spec_unchanged() {
        let doc = json!({
            "name": "a", "instruction": "b", "modality": "audio", "voice": "Puck",
            "greeting": "Hi."
        });
        assert!(plan(&doc, &Decisions::new()).questions.is_empty());
        let mut doc = doc;
        doc.as_object_mut().unwrap().remove("greeting");
        let out = answer(
            &doc,
            &[Answer::choose("greeting", "wait")],
            &Decisions::new(),
        )
        .unwrap();
        assert_eq!(out.spec, doc);
        assert!(plan(&out.spec, &out.decisions).questions.is_empty());
        assert_eq!(
            answer(
                &out.spec,
                &[Answer::choose("greeting", "wait")],
                &out.decisions
            ),
            Err(AnswerError::NotOpen("greeting".into()))
        );
    }

    #[test]
    fn bad_answers_change_nothing() {
        let doc = json!({ "modality": "audio", "instruction": "x" });
        let none = Decisions::new();
        assert_eq!(
            answer(&doc, &[Answer::choose("voice", "Nope")], &none).unwrap_err(),
            AnswerError::UnknownChoice {
                id: "voice".into(),
                choice: "Nope".into(),
                options: VOICES.iter().map(|v| (*v).to_string()).collect(),
            }
        );
        assert!(matches!(
            answer(&doc, &[Answer::with_value("name", json!(""))], &none),
            Err(AnswerError::MissingValue { .. })
        ));
        assert!(matches!(
            answer(&doc, &[Answer::with_value("name", json!(7))], &none),
            Err(AnswerError::Invalid { .. })
        ));
        // A later failure discards earlier answers.
        assert!(
            answer(
                &doc,
                &[
                    Answer::with_value("name", json!("a")),
                    Answer::choose("nope", "x")
                ],
                &none
            )
            .is_err()
        );
    }

    #[test]
    fn commit_gate_skips_read_only_tools() {
        let mut doc = booking();
        doc["conversation"]["stages"][1]["allow"] = json!(["book_table", "check_availability"]);
        let ids: Vec<String> = plan(&doc, &Decisions::new())
            .questions
            .into_iter()
            .map(|q| q.id)
            .collect();
        assert_eq!(ids, ["commit_gate:book_table"]);
        assert!(looks_committing(&SessionSpec::default(), "cancel-order"));
        assert!(!looks_committing(&SessionSpec::default(), "lookup_order"));
        assert!(looks_committing(&SessionSpec::default(), "bookTable"));
        assert!(looks_sensitive("card_number") && looks_sensitive("cardNumber"));
        assert!(looks_sensitive("dateOfBirth") && looks_sensitive("SSN"));
        assert!(!looks_sensitive("shipping_address") && !looks_sensitive("spinach"));
        assert_eq!(name_parts("bookTable-v2"), ["book", "table", "v2"]);
    }
}
