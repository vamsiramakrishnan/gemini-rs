//! `decisions`: the questions a decision model answers about the
//! conversation, named by `decided` guards.
//!
//! ```json
//! "decisions": {
//!   "confirmed": {
//!     "type": "boolean",
//!     "instructions": "In their last turn, did the caller agree to the booking that was read back?",
//!     "criteria": { "true": "the caller said yes in their own words",
//!                   "false": "they hesitated, changed a detail, or only picked an option" }
//!   },
//!   "picked_slot": {
//!     "type": "choice",
//!     "instructions": "Which of the offered times did the caller choose?",
//!     "options_from": "availability.slots",
//!     "none": "the caller has not chosen one of the offered times",
//!     "writes": "slot"
//!   }
//! },
//! "conversation": { "stages": [ { "id": "confirm", "commit": { "tool": "book", "when": { "decided": "confirmed" } } } ] }
//! ```
//!
//! Lowered to a [`Decisions`] bank over [`SpecResources::decision_model`](super::SpecResources::decision_model).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use gemini_adk_rs::decision::{BooleanCriteria, Decision, DecisionModel, Decisions, Question};
use gemini_adk_rs::flow::{Decided, Expect, Guard, Pred};

use super::SessionSpec;

/// One question a decision model answers about the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionSpec {
    /// The question and its options.
    #[serde(flatten)]
    pub kind: DecisionKind,
    /// For a choice: the state path (`key` or `key.field`) holding an array
    /// of options, read at each ask, such as the slots a tool offered. An
    /// object option is keyed by its `id`, `value` or `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options_from: Option<String>,
    /// For a choice: an extra option, `none_of_these`, with this
    /// description. A choice always picks something, so "which offered time
    /// did the caller pick?" needs a way to say they have not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub none: Option<String>,
    /// Boolean: the probability needed for yes (and `1 -` this for no,
    /// default 0.85). Choice and score: the certainty needed (default 0.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_least: Option<f64>,
    /// Write the decided value into this state key: the chosen option (an
    /// object option whole), the score, or the boolean. The question is then
    /// also asked whenever an active guard reads the key, so a stage that
    /// collects it is filled by the pick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writes: Option<String>,
}

/// The question types a decision model answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DecisionKind {
    /// Answered with the probability that it is true.
    Boolean {
        /// The question. Ask about "their last turn" for a confirmation or an
        /// intent, so an earlier answer is not read again.
        instructions: String,
        /// What the true and false cases mean.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<BooleanCriteriaSpec>,
    },
    /// Answered with one option.
    Choice {
        /// The question.
        instructions: String,
        /// Option name to description (1 to 255). May be empty with
        /// `options_from`.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        criteria: BTreeMap<String, String>,
    },
    /// Answered with a position on an ordered scale, from 0.
    Score {
        /// The question.
        instructions: String,
        /// Level descriptions, lowest first (2 to 10).
        criteria: Vec<String>,
    },
}

/// What the true and false cases of a boolean question mean.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BooleanCriteriaSpec {
    /// When it is true.
    #[serde(rename = "true")]
    pub when_true: String,
    /// When it is false.
    #[serde(rename = "false")]
    pub when_false: String,
}

impl DecisionSpec {
    /// The question as the decision model takes it.
    pub fn question(&self) -> Question {
        match &self.kind {
            DecisionKind::Boolean {
                instructions,
                criteria,
            } => Question::Boolean {
                instructions: instructions.clone(),
                criteria: criteria.as_ref().map(|c| BooleanCriteria {
                    when_true: c.when_true.clone(),
                    when_false: c.when_false.clone(),
                }),
            },
            DecisionKind::Choice {
                instructions,
                criteria,
            } => Question::Choice {
                instructions: instructions.clone(),
                criteria: criteria.clone(),
            },
            DecisionKind::Score {
                instructions,
                criteria,
            } => Question::Score {
                instructions: instructions.clone(),
                criteria: criteria.clone(),
            },
        }
    }

    /// The runtime form.
    pub fn compile(&self) -> Decision {
        let mut d = Decision::new(self.question());
        d.options_from.clone_from(&self.options_from);
        d.none.clone_from(&self.none);
        d.at_least = self.at_least;
        d.writes.clone_from(&self.writes);
        d
    }

    /// Problems with this question, prefixed with its id.
    pub fn problems(&self, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        let at = |m: &str| format!("decision '{id}': {m}");
        let mut question = self.question();
        // Options read from state are filled in at each ask.
        if let (Some(_), Question::Choice { criteria, .. }) = (&self.options_from, &mut question)
            && criteria.is_empty()
        {
            criteria.insert("_".into(), "_".into());
        }
        if let Err(e) = question.validate() {
            out.push(at(&e));
        }
        let choice = matches!(self.kind, DecisionKind::Choice { .. });
        if self.options_from.is_some() && !choice {
            out.push(at("options_from applies to choice questions only"));
        }
        if self.none.is_some() && !choice {
            out.push(at("none applies to choice questions only"));
        }
        if let Some(t) = self.at_least {
            let boolean = matches!(self.kind, DecisionKind::Boolean { .. });
            let ok = if boolean {
                t > 0.5 && t <= 1.0
            } else {
                t > 0.0 && t <= 1.0
            };
            if !ok {
                out.push(at(&format!(
                    "at_least {t} must be {}",
                    if boolean {
                        "above 0.5 and at most 1"
                    } else {
                        "above 0 and at most 1"
                    }
                )));
            }
        }
        if self.writes.as_deref().is_some_and(|k| k.trim().is_empty()) {
            out.push(at("writes is empty"));
        }
        out
    }

    /// Whether `expect` fits this question's type, and the option exists
    /// when the choice's options are fixed.
    fn accepts(&self, expect: &Expect) -> Result<(), String> {
        match (&self.kind, expect) {
            (_, Expect::Yes) => Ok(()),
            (DecisionKind::Boolean { .. }, Expect::No) => Ok(()),
            (DecisionKind::Choice { .. }, Expect::No) if self.none.is_some() => Ok(()),
            (DecisionKind::Choice { criteria, .. }, Expect::Is(option)) => {
                if self.options_from.is_some() || criteria.contains_key(option) {
                    Ok(())
                } else {
                    Err(format!(
                        "'{option}' is not one of its options ({})",
                        criteria.keys().cloned().collect::<Vec<_>>().join(", ")
                    ))
                }
            }
            (DecisionKind::Score { .. }, Expect::AtLeast(_) | Expect::AtMost(_)) => Ok(()),
            (DecisionKind::Choice { .. }, Expect::No) => {
                Err("false means \"none of these\", which needs a `none` option".into())
            }
            (kind, expect) => Err(format!(
                "a {} question cannot be expected to be {expect:?}",
                match kind {
                    DecisionKind::Boolean { .. } => "boolean",
                    DecisionKind::Choice { .. } => "choice",
                    DecisionKind::Score { .. } => "score",
                }
            )),
        }
    }
}

/// Every `decided` atom in a predicate.
fn atoms(pred: &Pred, out: &mut Vec<Decided>) {
    match pred {
        Pred::Decided(d) => out.push(d.clone()),
        Pred::All(ps) | Pred::Any(ps) => ps.iter().for_each(|p| atoms(p, out)),
        Pred::Not(p) => atoms(p, out),
        _ => {}
    }
}

fn guard_atoms(guard: &Guard, out: &mut Vec<Decided>) {
    if let Guard::Spec(p) = guard {
        atoms(p, out);
    }
}

impl SessionSpec {
    /// Every `decided` atom in the spec, with where it is, and whether that
    /// place is read outside a flow (phase transitions and patterns), where
    /// no flow names the question to ask.
    pub(crate) fn decided_atoms(&self) -> Vec<(String, Decided, bool)> {
        let mut out = Vec::new();
        let mut push = |place: String, g: &Guard, standing: bool| {
            let mut found = Vec::new();
            guard_atoms(g, &mut found);
            out.extend(found.into_iter().map(|d| (place.clone(), d, standing)));
        };
        if let Some(c) = &self.conversation {
            let stages = c
                .stages
                .iter()
                .map(|s| ("conversation".to_string(), s))
                .chain(c.overlays.iter().flat_map(|o| {
                    o.stages
                        .iter()
                        .map(move |s| (format!("digression '{}'", o.name), s))
                }));
            for (layer, s) in stages {
                if let Some(g) = &s.done {
                    push(format!("{layer} stage '{}' done", s.id), g, false);
                }
                if let Some(commit) = &s.commit {
                    push(
                        format!("{layer} stage '{}' commit", s.id),
                        &commit.when,
                        false,
                    );
                }
                for n in &s.next {
                    push(format!("{layer} stage '{}' next", s.id), &n.when, false);
                }
            }
            for o in &c.overlays {
                push(
                    format!("digression '{}' trigger", o.name),
                    &o.trigger,
                    false,
                );
            }
        }
        if let Some(flow) = &self.flow {
            for step in &flow.steps {
                for g in step.gate.iter().chain(step.done.iter()) {
                    push(format!("flow step '{}'", step.id), g, false);
                }
                for e in &step.after {
                    if let Some(w) = &e.when {
                        push(format!("flow step '{}' edge", step.id), w, false);
                    }
                }
            }
            for c in &flow.constraints {
                if let gemini_adk_rs::flow::Constraint::NeverUntil { tool, until } = c {
                    push(format!("flow constraint on '{tool}'"), until, false);
                }
            }
        }
        for p in &self.phases {
            for t in &p.transitions {
                push(format!("phase '{}' transition", p.name), &t.when, true);
            }
        }
        for p in &self.patterns {
            push(format!("pattern '{}'", p.name), &p.when, true);
        }
        out
    }

    /// Problems with the `decisions` bank and the `decided` atoms naming it.
    pub(crate) fn decision_problems(&self) -> (Vec<String>, Vec<String>) {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        for (id, spec) in &self.decisions {
            errors.extend(spec.problems(id));
        }
        let mut named = BTreeSet::new();
        for (place, atom, _) in self.decided_atoms() {
            named.insert(atom.question.clone());
            match self.decisions.get(&atom.question) {
                None => errors.push(format!(
                    "{place}: decided '{}' names no question in `decisions`",
                    atom.question
                )),
                Some(spec) => {
                    if let Err(e) = spec.accepts(&atom.expect) {
                        errors.push(format!("{place}: decided '{}': {e}", atom.question));
                    }
                }
            }
        }
        for (id, spec) in &self.decisions {
            if !named.contains(id) && spec.writes.is_none() {
                warnings.push(format!(
                    "decision '{id}' is never asked: no guard names it and it writes nothing"
                ));
            }
        }
        (errors, warnings)
    }

    /// The `decisions` bank as the runtime's [`Decisions`], over `model`.
    /// Questions read outside a flow (phase transitions, patterns) are
    /// standing: asked at every caller turn.
    pub fn compile_decisions(&self, model: Arc<dyn DecisionModel>) -> Decisions {
        let standing: BTreeSet<String> = self
            .decided_atoms()
            .into_iter()
            .filter(|(_, _, standing)| *standing)
            .map(|(_, d, _)| d.question)
            .collect();
        self.decisions
            .iter()
            .fold(Decisions::new(model), |d, (id, spec)| {
                d.question(id.clone(), spec.compile())
            })
            .standing(standing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(decisions: serde_json::Value, stages: serde_json::Value) -> SessionSpec {
        SessionSpec::from_value(json!({
            "name": "t",
            "instruction": "Book tables.",
            "tools": [{ "name": "book" }, { "name": "transfer" }],
            "decisions": decisions,
            "conversation": { "name": "c", "stages": stages,
                "overlays": [{ "name": "handoff", "trigger": { "decided": "wants_person" },
                               "stages": [{ "id": "transfer", "allow": ["transfer"],
                                            "done": { "called_ok": "transfer" } }],
                               "resume": "terminate" }] }
        }))
        .expect("parses")
    }

    fn bank() -> serde_json::Value {
        json!({
            "confirmed": { "type": "boolean", "instructions": "Agreed?",
                           "criteria": { "true": "said yes", "false": "anything else" } },
            "wants_person": { "type": "boolean", "instructions": "Asked for a person?" },
            "next_step": { "type": "choice", "instructions": "Next?",
                           "criteria": { "book": "confirmed", "stay": "not yet" } },
            "picked": { "type": "choice", "instructions": "Which time?",
                        "options_from": "availability.options", "none": "not picked",
                        "writes": "slot" },
            "anger": { "type": "score", "instructions": "How angry?",
                       "criteria": ["calm", "upset", "angry"] }
        })
    }

    fn stages() -> serde_json::Value {
        json!([
            { "id": "collect", "collect": ["slot"] },
            { "id": "confirm", "after": ["collect"],
              "commit": { "tool": "book", "when": { "decided": "confirmed" } },
              "next": [{ "to": "end", "when": { "decided": { "next_step": "book" } } }] },
            { "id": "end", "after": ["confirm"], "terminal": true }
        ])
    }

    #[test]
    fn a_bank_and_its_atoms_validate() {
        let s = spec(bank(), stages());
        let (errors, warnings) = s.decision_problems();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            warnings,
            ["decision 'anger' is never asked: no guard names it and it writes nothing"]
        );
        let back = serde_json::to_value(&s).unwrap();
        assert_eq!(back["decisions"]["picked"]["writes"], "slot");
        assert_eq!(
            back["conversation"]["stages"][1]["commit"]["when"],
            json!({ "decided": "confirmed" })
        );
    }

    #[test]
    fn atoms_are_checked_against_the_bank() {
        let s = spec(
            bank(),
            json!([
                { "id": "a", "done": { "decided": "missing" } },
                { "id": "b", "after": ["a"], "done": { "decided": { "next_step": "fly" } } },
                { "id": "c", "after": ["b"], "done": { "decided": { "anger": "high" } } },
                { "id": "d", "after": ["c"], "done": { "decided": { "next_step": false } } },
                { "id": "e", "after": ["d"], "terminal": true, "done": { "decided": { "anger": { "at_least": 1 } } } }
            ]),
        );
        let (errors, _) = s.decision_problems();
        let joined = errors.join("\n");
        assert!(
            joined.contains("decided 'missing' names no question"),
            "{joined}"
        );
        assert!(
            joined.contains("'fly' is not one of its options"),
            "{joined}"
        );
        assert!(
            joined.contains("a score question cannot be expected"),
            "{joined}"
        );
        assert!(joined.contains("needs a `none` option"), "{joined}");
        assert_eq!(errors.len(), 4, "{joined}");
    }

    #[test]
    fn question_problems_are_reported() {
        let mut b = bank();
        b["anger"]["criteria"] = json!(["only one"]);
        b["confirmed"]["at_least"] = json!(0.4);
        b["wants_person"]["options_from"] = json!("x");
        let (errors, _) = spec(b, stages()).decision_problems();
        let joined = errors.join("\n");
        assert!(
            joined.contains("score question takes 2 to 10 levels"),
            "{joined}"
        );
        assert!(
            joined.contains("at_least 0.4 must be above 0.5"),
            "{joined}"
        );
        assert!(
            joined.contains("options_from applies to choice"),
            "{joined}"
        );
    }

    #[test]
    fn questions_read_outside_a_flow_are_standing() {
        let s = SessionSpec::from_value(json!({
            "name": "t",
            "instruction": "x",
            "decisions": { "angry": { "type": "boolean", "instructions": "Angry?" } },
            "phases": [{ "name": "main",
                         "transitions": [{ "to": "calm", "when": { "decided": "angry" } }] },
                       { "name": "calm" }],
            "initial_phase": "main"
        }))
        .unwrap();
        let model = Arc::new(gemini_adk_rs::decision::MockDecisionModel::new(|_| {
            Ok(Default::default())
        }));
        let d = s.compile_decisions(model);
        let scope = gemini_adk_rs::flow::DecisionScope::default();
        assert_eq!(d.select(&scope), ["angry".to_string()].into());
    }
}
