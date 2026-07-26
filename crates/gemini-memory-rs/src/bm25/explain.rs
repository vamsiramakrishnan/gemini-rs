//! Search explanation.
//!
//! "Why did B say that?" has to be answerable. Every hit carries the term-level
//! and boost-level breakdown that produced its score, so a surprising retrieval
//! can be traced to a field match or a ranking signal rather than guessed at.

use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

use super::schema::Field;
use crate::core::MemoryId;

/// One term's contribution from one field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreComponent {
    /// The matched term.
    pub term: String,
    /// The field it matched in.
    pub field: Field,
    /// Weighted BM25 contribution.
    pub score: f32,
}

/// A non-lexical ranking signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoostKind {
    /// The query named this record's subject or an entity it mentions.
    ExactEntity,
    /// The record rests on something the user said outright.
    ExplicitSource,
    /// The record was learned in the current session.
    SessionOverlay,
    /// Scaled by the record's aggregated confidence.
    Confidence,
    /// Applied to records below the confidence floor.
    LowConfidencePenalty,
    /// Time decay for episodic and non-persistent records.
    Recency,
}

impl BoostKind {
    /// A short label for rendered explanations.
    pub fn label(self) -> &'static str {
        match self {
            Self::ExactEntity => "exact entity match",
            Self::ExplicitSource => "explicit source",
            Self::SessionOverlay => "current-session fact",
            Self::Confidence => "confidence",
            Self::LowConfidencePenalty => "low-confidence penalty",
            Self::Recency => "recency",
        }
    }
}

/// The full derivation of one hit's score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchExplanation {
    /// Which record this explains.
    pub memory_id: MemoryId,
    /// Per-term, per-field lexical contributions.
    pub components: Vec<ScoreComponent>,
    /// Ranking signals applied on top.
    pub boosts: Vec<(BoostKind, f32)>,
    /// Sum of the lexical contributions.
    pub lexical_score: f32,
    /// Score after boosts.
    pub final_score: f32,
}

impl SearchExplanation {
    /// Render the derivation as aligned text, for `explain-search` output.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "candidate {}:", self.memory_id);
        let mut components = self.components.clone();
        components.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for component in &components {
            let _ = writeln!(
                out,
                "  {:<28} {:>6.2}",
                format!("{} ({})", component.term, component.field.label()),
                component.score
            );
        }
        let _ = writeln!(
            out,
            "  {:<28} {:>6.2}",
            "lexical subtotal", self.lexical_score
        );
        for (kind, amount) in &self.boosts {
            let _ = writeln!(out, "  {:<28} {:>+6.2}", kind.label(), amount);
        }
        let _ = writeln!(out, "  {:<28} {:>6.2}", "final score", self.final_score);
        out
    }

    /// The single strongest lexical contribution, if any.
    pub fn top_component(&self) -> Option<&ScoreComponent> {
        self.components.iter().max_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn explanation() -> SearchExplanation {
        SearchExplanation {
            memory_id: MemoryId::new("mem_17"),
            components: vec![
                ScoreComponent {
                    term: "restaurant".into(),
                    field: Field::Statement,
                    score: 5.9,
                },
                ScoreComponent {
                    term: "wife".into(),
                    field: Field::Aliases,
                    score: 2.5,
                },
            ],
            boosts: vec![(BoostKind::ExactEntity, 3.0), (BoostKind::Confidence, 0.4)],
            lexical_score: 8.4,
            final_score: 11.8,
        }
    }

    #[test]
    fn renders_a_readable_derivation() {
        let rendered = explanation().render();
        assert!(rendered.contains("candidate mem_17:"));
        assert!(rendered.contains("restaurant (statement)"));
        assert!(rendered.contains("exact entity match"));
        assert!(rendered.contains("final score"));
        // Strongest contribution is listed first.
        let restaurant = rendered.find("restaurant").unwrap();
        let wife = rendered.find("wife").unwrap();
        assert!(restaurant < wife);
    }

    #[test]
    fn reports_the_strongest_component() {
        assert_eq!(explanation().top_component().unwrap().term, "restaurant");
    }
}
