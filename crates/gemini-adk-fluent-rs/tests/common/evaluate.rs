//! Scoring a run: functional requirements, non-functional requirements, and
//! adversarial outcomes.
//!
//! # Why these three are kept apart
//!
//! They fail differently and a single pass/fail number would hide that.
//!
//! **Functional** requirements are binary and mechanically checkable. Either
//! `charge_card` ran before `identity_verified` was set or it did not; there is
//! no judgement and no threshold. A functional failure is a defect.
//!
//! **Non-functional** requirements are measurements against a stated budget.
//! "Turn latency p95 under 8 s" is a number that moves with the network, the
//! model and the day. A non-functional failure is a *signal*, and the report
//! prints the measurement next to the budget so a reader can tell a miss by
//! 5% from a miss by 5×.
//!
//! **Adversarial** outcomes are neither. They ask whether a specific attack
//! succeeded, and the interesting axis is not pass/fail but *which surface*
//! gave way: the flow refusing a tool is the system working; the model saying
//! something it should not have is the system failing in a way no tool gate can
//! catch. Conflating the two would let a run look green while the assistant
//! reads a stranger their balance.
//!
//! # On what is asserted versus reported
//!
//! A live model over a live network is not deterministic, and an evaluation
//! that fails for reasons nobody can act on gets rerun until green — which is
//! how a real regression gets waved through. So the harness **asserts** only
//! what is unambiguous (a tool ran, a gate refused, an order was violated) and
//! **reports** everything else (what was said, how long it took, which phrasing
//! the recogniser produced).

#![allow(dead_code)]

use std::fmt;

/// How a single requirement came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The requirement held.
    Pass,
    /// The requirement was violated.
    Fail,
    /// The run did not reach the point where this could be judged — reported
    /// as its own state rather than silently counted as a pass, because "we
    /// never got there" and "it behaved correctly" are different facts.
    NotReached,
}

impl Outcome {
    /// The symbol used in the rendered report.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::NotReached => "N/R",
        }
    }

    /// Whether this outcome should fail the suite.
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Fail)
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.glyph())
    }
}

/// A functional requirement: binary, mechanically checkable, no threshold.
#[derive(Debug, Clone)]
pub struct FunctionalResult {
    /// Stable identifier, e.g. `FR-1`.
    pub id: &'static str,
    /// What the requirement demands, in one line.
    pub requirement: &'static str,
    /// Why it exists — the regulation or the incident it prevents.
    pub rationale: &'static str,
    /// How it came out.
    pub outcome: Outcome,
    /// The observation behind the verdict, so a reader need not trust it.
    pub evidence: String,
}

/// A non-functional requirement: a measurement against a stated budget.
#[derive(Debug, Clone)]
pub struct NonFunctionalResult {
    /// Stable identifier, e.g. `NFR-1`.
    pub id: &'static str,
    /// What is being measured.
    pub metric: &'static str,
    /// Why this number matters to a caller on a phone.
    pub rationale: &'static str,
    /// The measured value, pre-formatted with its unit.
    pub measured: String,
    /// The budget it is judged against, pre-formatted.
    pub budget: String,
    /// How it came out.
    pub outcome: Outcome,
}

/// Which surface an attack tested, and therefore what a success would mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// The flow's tool gate — enforced by `admits_tool`, not by persuasion.
    /// A failure here means the governance model is broken.
    FlowGate,
    /// What the model chose to say. Nothing mechanically prevents speech, so a
    /// failure here is a prompt/behaviour finding rather than a gate defect —
    /// and is not fixable by adding constraints to the DAG.
    ModelSpeech,
}

impl Surface {
    /// Label used in the report.
    pub fn label(self) -> &'static str {
        match self {
            Self::FlowGate => "flow gate",
            Self::ModelSpeech => "model speech",
        }
    }
}

/// The result of one adversarial probe.
#[derive(Debug, Clone)]
pub struct AdversarialResult {
    /// Stable identifier, e.g. `ADV-1`.
    pub id: &'static str,
    /// A short name for the attack.
    pub name: &'static str,
    /// What the caller actually said, verbatim — the report is not useful
    /// without it.
    pub utterance: String,
    /// The rule the attack tries to break.
    pub rule: &'static str,
    /// Which surface is under test.
    pub surface: Surface,
    /// Whether the system held.
    pub outcome: Outcome,
    /// What the assistant said back, as the recogniser heard it.
    pub response: String,
    /// The observation behind the verdict.
    pub evidence: String,
}

/// Latency samples for one session, in milliseconds.
#[derive(Debug, Default, Clone)]
pub struct Latencies {
    /// Time from the caller finishing speaking to the first audio byte back.
    pub first_audio_ms: Vec<u128>,
    /// Time from the caller finishing speaking to the turn completing.
    pub turn_ms: Vec<u128>,
}

impl Latencies {
    /// Percentile over a copy of the samples. `p` in 0..=100.
    ///
    /// Nearest-rank, which is the honest choice at these sample counts: with
    /// six turns, interpolating between them would invent precision the data
    /// does not have.
    pub fn percentile(samples: &[u128], p: usize) -> Option<u128> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let rank = (p * sorted.len()).div_ceil(100).max(1) - 1;
        sorted.get(rank).copied()
    }

    /// p50 of turn latency.
    pub fn turn_p50(&self) -> Option<u128> {
        Self::percentile(&self.turn_ms, 50)
    }

    /// p95 of turn latency.
    pub fn turn_p95(&self) -> Option<u128> {
        Self::percentile(&self.turn_ms, 95)
    }

    /// p50 of time-to-first-audio.
    pub fn first_audio_p50(&self) -> Option<u128> {
        Self::percentile(&self.first_audio_ms, 50)
    }
}

/// Judge a measurement against a budget, reporting `NotReached` when there is
/// nothing to judge rather than passing an empty sample set.
pub fn within(measured: Option<u128>, budget_ms: u128) -> Outcome {
    match measured {
        Some(v) if v <= budget_ms => Outcome::Pass,
        Some(_) => Outcome::Fail,
        None => Outcome::NotReached,
    }
}

/// Format an optional millisecond measurement for the report.
pub fn ms(value: Option<u128>) -> String {
    match value {
        Some(v) if v >= 1000 => format!("{:.1} s", v as f64 / 1000.0),
        Some(v) => format!("{v} ms"),
        None => "—".to_string(),
    }
}

/// The complete result of an evaluation run.
#[derive(Debug, Default)]
pub struct Evaluation {
    /// Functional requirement results.
    pub functional: Vec<FunctionalResult>,
    /// Non-functional requirement results.
    pub non_functional: Vec<NonFunctionalResult>,
    /// Adversarial probe results.
    pub adversarial: Vec<AdversarialResult>,
    /// Free-text notes about how the run was conducted.
    pub notes: Vec<String>,
    /// What the run did **not** establish.
    ///
    /// A report that lists only what it proved reads as if it proved
    /// everything. Anything observed but not diagnosed belongs here, in the
    /// output, rather than in a commit message nobody reads — otherwise the
    /// next person to run this rediscovers it.
    pub unresolved: Vec<String>,
}

impl Evaluation {
    /// Every functional or adversarial failure. Non-functional misses are
    /// excluded deliberately: a latency budget missed on a shared runner is not
    /// a defect, and treating it as one trains people to ignore the report.
    pub fn blocking_failures(&self) -> Vec<String> {
        let mut out = Vec::new();
        for f in &self.functional {
            if f.outcome.is_failure() {
                out.push(format!("{}: {}", f.id, f.requirement));
            }
        }
        for a in &self.adversarial {
            if a.outcome.is_failure() {
                out.push(format!("{} ({}): {}", a.id, a.surface.label(), a.name));
            }
        }
        out
    }

    /// Counts of pass / fail / not-reached across functional and adversarial.
    pub fn tally(&self) -> (usize, usize, usize) {
        let outcomes = self
            .functional
            .iter()
            .map(|f| f.outcome)
            .chain(self.adversarial.iter().map(|a| a.outcome));
        let mut pass = 0;
        let mut fail = 0;
        let mut not_reached = 0;
        for o in outcomes {
            match o {
                Outcome::Pass => pass += 1,
                Outcome::Fail => fail += 1,
                Outcome::NotReached => not_reached += 1,
            }
        }
        (pass, fail, not_reached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank() {
        let s = vec![10u128, 20, 30, 40];
        assert_eq!(Latencies::percentile(&s, 50), Some(20));
        assert_eq!(Latencies::percentile(&s, 95), Some(40));
        assert_eq!(Latencies::percentile(&s, 100), Some(40));
        assert_eq!(Latencies::percentile(&[], 50), None);
    }

    #[test]
    fn an_unmeasured_budget_is_not_a_pass() {
        // Silently passing an empty sample set is how a suite reports green on
        // a run that never exercised the thing.
        assert_eq!(within(None, 1000), Outcome::NotReached);
        assert_eq!(within(Some(999), 1000), Outcome::Pass);
        assert_eq!(within(Some(1001), 1000), Outcome::Fail);
    }

    #[test]
    fn only_functional_and_adversarial_failures_block() {
        let mut eval = Evaluation::default();
        eval.non_functional.push(NonFunctionalResult {
            id: "NFR-1",
            metric: "turn latency p95",
            rationale: "",
            measured: "9 s".into(),
            budget: "8 s".into(),
            outcome: Outcome::Fail,
        });
        assert!(
            eval.blocking_failures().is_empty(),
            "a latency miss on a shared runner must not be reported as a defect"
        );
        eval.functional.push(FunctionalResult {
            id: "FR-1",
            requirement: "no payment before verification",
            rationale: "",
            outcome: Outcome::Fail,
            evidence: String::new(),
        });
        assert_eq!(eval.blocking_failures().len(), 1);
    }
}
