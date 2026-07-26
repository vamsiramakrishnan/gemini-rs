//! Retrieval quality metrics.
//!
//! Ordinary information-retrieval measures, defined here rather than pulled in
//! so the eval harness has no dependency of its own and the definitions are
//! visible next to the thresholds they are judged against.

/// Fraction of returned results that were relevant.
///
/// Returning nothing is treated as perfect precision — it is a recall failure,
/// and counting it twice would hide which of the two actually went wrong.
pub fn precision(returned: &[String], relevant: &[String]) -> f32 {
    if returned.is_empty() {
        return 1.0;
    }
    let hits = returned.iter().filter(|r| relevant.contains(r)).count();
    hits as f32 / returned.len() as f32
}

/// Fraction of relevant results that were returned.
pub fn recall(returned: &[String], relevant: &[String]) -> f32 {
    if relevant.is_empty() {
        return 1.0;
    }
    let hits = relevant.iter().filter(|r| returned.contains(r)).count();
    hits as f32 / relevant.len() as f32
}

/// Reciprocal of the rank of the first relevant result.
pub fn reciprocal_rank(returned: &[String], relevant: &[String]) -> f32 {
    returned
        .iter()
        .position(|r| relevant.contains(r))
        .map(|idx| 1.0 / (idx as f32 + 1.0))
        .unwrap_or(0.0)
}

/// Normalized discounted cumulative gain over binary relevance.
pub fn ndcg(returned: &[String], relevant: &[String]) -> f32 {
    if relevant.is_empty() {
        return 1.0;
    }
    let dcg: f32 = returned
        .iter()
        .enumerate()
        .map(|(idx, id)| {
            if relevant.contains(id) {
                1.0 / ((idx as f32 + 2.0).log2())
            } else {
                0.0
            }
        })
        .sum();
    let ideal: f32 = (0..relevant.len().min(returned.len().max(1)))
        .map(|idx| 1.0 / ((idx as f32 + 2.0).log2()))
        .sum();
    if ideal == 0.0 {
        0.0
    } else {
        (dcg / ideal).min(1.0)
    }
}

/// Mean of a set of per-case scores.
pub fn mean(scores: &[f32]) -> f32 {
    if scores.is_empty() {
        return 0.0;
    }
    scores.iter().sum::<f32>() / scores.len() as f32
}

/// The `p`th percentile of a sample, by nearest rank.
pub fn percentile(samples: &mut [f32], p: f32) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((p / 100.0) * samples.len() as f32).ceil().max(1.0) as usize;
    samples[rank.min(samples.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_string()).collect()
    }

    #[test]
    fn precision_and_recall_pull_apart_the_two_failure_modes() {
        let returned = ids(&["a", "b", "c", "d"]);
        let relevant = ids(&["a", "b"]);
        assert_eq!(precision(&returned, &relevant), 0.5);
        assert_eq!(recall(&returned, &relevant), 1.0);

        let stingy = ids(&["a"]);
        assert_eq!(precision(&stingy, &relevant), 1.0);
        assert_eq!(recall(&stingy, &relevant), 0.5);
    }

    #[test]
    fn returning_nothing_is_a_recall_failure_not_a_precision_one() {
        let relevant = ids(&["a"]);
        assert_eq!(precision(&[], &relevant), 1.0);
        assert_eq!(recall(&[], &relevant), 0.0);
    }

    #[test]
    fn reciprocal_rank_rewards_ranking_the_answer_first() {
        let relevant = ids(&["c"]);
        assert_eq!(reciprocal_rank(&ids(&["c", "a", "b"]), &relevant), 1.0);
        assert!((reciprocal_rank(&ids(&["a", "c", "b"]), &relevant) - 0.5).abs() < 1e-6);
        assert_eq!(reciprocal_rank(&ids(&["a", "b"]), &relevant), 0.0);
    }

    #[test]
    fn ndcg_prefers_relevant_results_earlier() {
        let relevant = ids(&["a", "b"]);
        let good = ndcg(&ids(&["a", "b", "x"]), &relevant);
        let worse = ndcg(&ids(&["x", "a", "b"]), &relevant);
        assert!(good > worse);
        assert!(good <= 1.0);
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        let mut samples = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert_eq!(percentile(&mut samples, 50.0), 5.0);
        assert_eq!(percentile(&mut samples, 95.0), 10.0);
        assert_eq!(percentile(&mut [], 95.0), 0.0);
    }

    #[test]
    fn mean_of_nothing_is_zero() {
        assert_eq!(mean(&[]), 0.0);
        assert_eq!(mean(&[1.0, 3.0]), 2.0);
    }
}
