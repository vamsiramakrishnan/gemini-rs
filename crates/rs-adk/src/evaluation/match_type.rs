//! Trajectory match types — control how tool call sequences are compared.

use serde::{Deserialize, Serialize};

/// How to compare actual vs. expected tool call trajectories.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrajectoryMatchType {
    /// Exact match: same tools in the same order, same count.
    #[default]
    Exact,
    /// In-order match: expected tools appear in order within the actual sequence
    /// (extra actual calls are allowed).
    InOrder,
    /// Any-order match: all expected tools appear somewhere in the actual sequence,
    /// regardless of ordering.
    AnyOrder,
}

impl TrajectoryMatchType {
    /// Score two tool-name sequences according to this match type.
    ///
    /// Returns `(score, explanation)` where score is in `[0.0, 1.0]`.
    pub fn score(&self, actual: &[String], expected: &[String]) -> (f64, String) {
        if expected.is_empty() && actual.is_empty() {
            return (1.0, "Both empty — trivially matching".into());
        }
        if expected.is_empty() {
            return (1.0, "No expected tools — any trajectory acceptable".into());
        }

        match self {
            Self::Exact => {
                if actual == expected {
                    (1.0, "Exact trajectory match".into())
                } else {
                    // Partial credit via LCS ratio
                    let lcs = lcs_length(actual, expected);
                    let max_len = actual.len().max(expected.len());
                    let score = lcs as f64 / max_len as f64;
                    (
                        score,
                        format!(
                            "Exact mismatch: LCS {lcs}/{max_len} (actual={}, expected={})",
                            actual.len(),
                            expected.len()
                        ),
                    )
                }
            }
            Self::InOrder => {
                // Check if expected is a subsequence of actual
                let mut ei = 0;
                for a in actual {
                    if ei < expected.len() && a == &expected[ei] {
                        ei += 1;
                    }
                }
                let matched = ei;
                let score = matched as f64 / expected.len() as f64;
                (
                    score,
                    format!(
                        "In-order: {matched}/{} expected tools found in sequence",
                        expected.len()
                    ),
                )
            }
            Self::AnyOrder => {
                let expected_set: std::collections::HashSet<&str> =
                    expected.iter().map(|s| s.as_str()).collect();
                let actual_set: std::collections::HashSet<&str> =
                    actual.iter().map(|s| s.as_str()).collect();
                let found = expected_set.intersection(&actual_set).count();
                let score = found as f64 / expected_set.len() as f64;
                (
                    score,
                    format!(
                        "Any-order: {found}/{} expected tools found",
                        expected_set.len()
                    ),
                )
            }
        }
    }
}

/// Compute length of longest common subsequence.
fn lcs_length(a: &[String], b: &[String]) -> usize {
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            if a[i - 1] == b[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    dp[m][n]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn exact_match() {
        let (score, _) =
            TrajectoryMatchType::Exact.score(&names(&["a", "b", "c"]), &names(&["a", "b", "c"]));
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn exact_mismatch_partial_credit() {
        let (score, _) =
            TrajectoryMatchType::Exact.score(&names(&["a", "c"]), &names(&["a", "b", "c"]));
        assert!(score > 0.0);
        assert!(score < 1.0);
    }

    #[test]
    fn in_order_subsequence() {
        let (score, _) = TrajectoryMatchType::InOrder
            .score(&names(&["a", "x", "b", "y", "c"]), &names(&["a", "b", "c"]));
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn in_order_partial() {
        // actual=["a","c"], expected=["a","b","c"]
        // "a" matches expected[0], "c" does not match expected[1]="b" => only 1/3
        let (score, _) =
            TrajectoryMatchType::InOrder.score(&names(&["a", "c"]), &names(&["a", "b", "c"]));
        assert!((score - 1.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn in_order_partial_with_extra() {
        // actual=["a","x","b","c"], expected=["a","b","c"]
        // "a" matches, skip "x", "b" matches, "c" matches => 3/3
        let (score, _) = TrajectoryMatchType::InOrder
            .score(&names(&["a", "x", "b", "c"]), &names(&["a", "b", "c"]));
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn any_order_full() {
        let (score, _) =
            TrajectoryMatchType::AnyOrder.score(&names(&["c", "a", "b"]), &names(&["a", "b", "c"]));
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn any_order_partial() {
        let (score, _) = TrajectoryMatchType::AnyOrder.score(&names(&["a"]), &names(&["a", "b"]));
        assert!((score - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn empty_both() {
        let (score, _) = TrajectoryMatchType::Exact.score(&[], &[]);
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn empty_expected() {
        let (score, _) = TrajectoryMatchType::Exact.score(&names(&["a"]), &[]);
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn default_is_exact() {
        assert_eq!(TrajectoryMatchType::default(), TrajectoryMatchType::Exact);
    }
}
