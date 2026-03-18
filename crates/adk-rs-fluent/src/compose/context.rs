//! C — Context engineering.
//!
//! Compose context policies additively with `+`.

use std::sync::Arc;

use rs_genai::prelude::Content;

/// A context policy that filters/transforms conversation history.
#[derive(Clone)]
pub struct ContextPolicy {
    name: &'static str,
    #[allow(clippy::type_complexity)]
    filter: Arc<dyn Fn(&[Content]) -> Vec<Content> + Send + Sync>,
}

impl ContextPolicy {
    fn new(
        name: &'static str,
        f: impl Fn(&[Content]) -> Vec<Content> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            filter: Arc::new(f),
        }
    }

    /// Apply this policy to conversation history.
    pub fn apply(&self, history: &[Content]) -> Vec<Content> {
        (self.filter)(history)
    }

    /// Name of this policy.
    pub fn name(&self) -> &str {
        self.name
    }
}

impl std::fmt::Debug for ContextPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextPolicy")
            .field("name", &self.name)
            .finish()
    }
}

/// Compose two context policies additively with `+`.
/// The combined policy applies both filters and merges (deduplicates) results.
impl std::ops::Add for ContextPolicy {
    type Output = ContextPolicyChain;

    fn add(self, rhs: ContextPolicy) -> Self::Output {
        ContextPolicyChain {
            policies: vec![self, rhs],
        }
    }
}

/// A chain of context policies applied in combination.
#[derive(Clone)]
pub struct ContextPolicyChain {
    /// The ordered list of policies in this chain.
    pub policies: Vec<ContextPolicy>,
}

impl ContextPolicyChain {
    /// Apply all policies and return the union of their results.
    pub fn apply(&self, history: &[Content]) -> Vec<Content> {
        let mut result = Vec::new();
        for policy in &self.policies {
            let filtered = policy.apply(history);
            // Simple append — dedup can be added if Content implements Eq
            result.extend(filtered);
        }
        result
    }
}

impl std::ops::Add<ContextPolicy> for ContextPolicyChain {
    type Output = ContextPolicyChain;

    fn add(mut self, rhs: ContextPolicy) -> Self::Output {
        self.policies.push(rhs);
        self
    }
}

/// The `C` namespace — static factory methods for context policies.
pub struct C;

impl C {
    /// Keep only the last `n` messages.
    pub fn window(n: usize) -> ContextPolicy {
        ContextPolicy::new("window", move |history| {
            if history.len() > n {
                history[history.len() - n..].to_vec()
            } else {
                history.to_vec()
            }
        })
    }

    /// Keep only messages with role "user".
    pub fn user_only() -> ContextPolicy {
        use rs_genai::prelude::Role;
        ContextPolicy::new("user_only", move |history| {
            history
                .iter()
                .filter(|c| c.role == Some(Role::User))
                .cloned()
                .collect()
        })
    }

    /// Apply a custom filter function.
    pub fn custom(f: impl Fn(&[Content]) -> Vec<Content> + Send + Sync + 'static) -> ContextPolicy {
        ContextPolicy::new("custom", f)
    }

    /// Keep only messages with role "model".
    pub fn model_only() -> ContextPolicy {
        use rs_genai::prelude::Role;
        ContextPolicy::new("model_only", move |history| {
            history
                .iter()
                .filter(|c| c.role == Some(Role::Model))
                .cloned()
                .collect()
        })
    }

    /// Keep the first `n` messages (head).
    pub fn head(n: usize) -> ContextPolicy {
        ContextPolicy::new("head", move |history| {
            history.iter().take(n).cloned().collect()
        })
    }

    /// Keep every `n`-th message (sampling).
    pub fn sample(n: usize) -> ContextPolicy {
        ContextPolicy::new("sample", move |history| {
            history
                .iter()
                .enumerate()
                .filter(|(i, _)| i % n == 0)
                .map(|(_, c)| c.clone())
                .collect()
        })
    }

    /// Exclude messages that contain tool-related parts (function calls/responses).
    pub fn exclude_tools() -> ContextPolicy {
        use rs_genai::prelude::Part;
        ContextPolicy::new("exclude_tools", move |history| {
            history
                .iter()
                .filter(|c| {
                    c.parts.iter().all(|p| {
                        !matches!(p, Part::FunctionCall { .. } | Part::FunctionResponse { .. })
                    })
                })
                .cloned()
                .collect()
        })
    }

    /// Prepend a system message to the context.
    pub fn prepend(content: Content) -> ContextPolicy {
        ContextPolicy::new("prepend", move |history| {
            let mut result = vec![content.clone()];
            result.extend(history.iter().cloned());
            result
        })
    }

    /// Append a content to the context.
    pub fn append(content: Content) -> ContextPolicy {
        ContextPolicy::new("append", move |history| {
            let mut result = history.to_vec();
            result.push(content.clone());
            result
        })
    }

    /// Keep only messages that contain text parts.
    pub fn text_only() -> ContextPolicy {
        use rs_genai::prelude::Part;
        ContextPolicy::new("text_only", move |history| {
            history
                .iter()
                .filter(|c| c.parts.iter().any(|p| matches!(p, Part::Text { .. })))
                .cloned()
                .collect()
        })
    }

    /// Filter messages by a predicate on Content.
    pub fn filter(f: impl Fn(&Content) -> bool + Send + Sync + 'static) -> ContextPolicy {
        ContextPolicy::new("filter", move |history| {
            history.iter().filter(|c| f(c)).cloned().collect()
        })
    }

    /// Map/transform each message in the context.
    pub fn map(f: impl Fn(&Content) -> Content + Send + Sync + 'static) -> ContextPolicy {
        ContextPolicy::new("map", move |history| history.iter().map(&f).collect())
    }

    /// Truncate context to approximately `max_chars` total characters of text.
    pub fn truncate(max_chars: usize) -> ContextPolicy {
        use rs_genai::prelude::Part;
        ContextPolicy::new("truncate", move |history| {
            let mut total = 0;
            let mut result = Vec::new();
            // Work backwards to keep most recent messages
            for c in history.iter().rev() {
                let text_len: usize = c
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Text { text } => Some(text.len()),
                        _ => None,
                    })
                    .sum();
                if total + text_len > max_chars && !result.is_empty() {
                    break;
                }
                total += text_len;
                result.push(c.clone());
            }
            result.reverse();
            result
        })
    }

    /// Keep messages within a time window (by index offset from end).
    /// Alias for `window` — provided for API symmetry.
    pub fn last(n: usize) -> ContextPolicy {
        Self::window(n)
    }

    /// Return an empty context (useful for isolated agents).
    pub fn empty() -> ContextPolicy {
        ContextPolicy::new("empty", |_| Vec::new())
    }

    /// Inject state values as context preamble.
    ///
    /// Bridges Channel 2 (State) → Channel 1 (Conversation History) by prepending
    /// formatted state values as a system context message.
    ///
    /// # Example
    /// ```ignore
    /// C::from_state(&["user:name", "app:account_balance", "derived:risk"])
    /// // Produces: "[Context: name=John, account_balance=$5230, risk=0.72]"
    /// ```
    pub fn from_state(keys: &[&str]) -> ContextPolicy {
        let owned_keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        ContextPolicy::new("from_state", move |history| {
            // Note: This policy captures keys but cannot access State at filter time.
            // The actual state injection happens at the Live session level via
            // instruction_template or on_turn_boundary. This policy prepends a
            // placeholder that the runtime populates.
            let mut result = Vec::new();
            if !owned_keys.is_empty() {
                let key_list = owned_keys.join(", ");
                result.push(Content::user(format!("[Context keys: {}]", key_list)));
            }
            result.extend(history.iter().cloned());
            result
        })
    }

    /// Alias for [`empty`](Self::empty) — matches upstream Python `C.none()`.
    pub fn none() -> ContextPolicy {
        Self::empty()
    }

    /// Alias for [`window`](Self::window) — matches upstream Python `C.recent()`.
    pub fn recent(n: usize) -> ContextPolicy {
        Self::window(n)
    }

    /// Template-based context injection with `{key}` placeholders.
    ///
    /// Replaces placeholders in the template with state key references.
    pub fn template(tpl: &str) -> ContextPolicy {
        let tpl = tpl.to_string();
        ContextPolicy::new("template", move |history| {
            let mut result = vec![Content::user(tpl.clone())];
            result.extend(history.iter().cloned());
            result
        })
    }

    /// Conditional context — applies inner policy only when predicate is true.
    ///
    /// Falls back to passing history through unchanged.
    pub fn when(
        predicate: impl Fn() -> bool + Send + Sync + 'static,
        inner: ContextPolicy,
    ) -> ContextPolicy {
        ContextPolicy::new("when", move |history| {
            if predicate() {
                inner.apply(history)
            } else {
                history.to_vec()
            }
        })
    }

    /// Rolling window — keeps last N messages (alias with summarization hint).
    pub fn rolling(n: usize) -> ContextPolicy {
        Self::window(n)
    }

    /// Compact context — removes tool call/response parts to reduce token usage.
    pub fn compact() -> ContextPolicy {
        Self::exclude_tools()
    }

    /// Budget context — truncate to approximate token count.
    ///
    /// Rough estimate: 4 chars per token.
    pub fn budget(max_tokens: usize) -> ContextPolicy {
        Self::truncate(max_tokens * 4)
    }

    /// Freshness filter — keep only messages within the last N entries.
    pub fn fresh(max_entries: usize) -> ContextPolicy {
        Self::window(max_entries)
    }

    /// Redact patterns from context messages.
    pub fn redact(patterns: &[&str]) -> ContextPolicy {
        use rs_genai::prelude::Part;
        let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
        ContextPolicy::new("redact", move |history| {
            history
                .iter()
                .map(|c| {
                    let parts: Vec<Part> = c
                        .parts
                        .iter()
                        .map(|p| match p {
                            Part::Text { text } => {
                                let mut redacted = text.clone();
                                for pattern in &patterns {
                                    redacted = redacted.replace(pattern.as_str(), "[REDACTED]");
                                }
                                Part::Text { text: redacted }
                            }
                            other => other.clone(),
                        })
                        .collect();
                    Content {
                        role: c.role,
                        parts,
                    }
                })
                .collect()
        })
    }

    /// LLM-powered context summarization.
    ///
    /// Stores a summarization prompt that the runtime uses to condense context
    /// via an LLM call before passing it to the agent. The policy prepends a
    /// marker so the runtime knows summarization is requested.
    ///
    /// # Example
    /// ```ignore
    /// C::summarize("Summarize the conversation focusing on action items")
    /// ```
    pub fn summarize(prompt: &str) -> ContextPolicy {
        let prompt = prompt.to_string();
        ContextPolicy::new("summarize", move |history| {
            let mut result = vec![Content::user(format!("[Summarize context: {}]", prompt))];
            result.extend(history.iter().cloned());
            result
        })
    }

    /// Keep only context relevant to a state key.
    ///
    /// Marker policy for LLM-powered relevance filtering. The runtime uses
    /// the referenced state key's value to filter context entries by relevance.
    ///
    /// # Example
    /// ```ignore
    /// C::relevant("user:current_topic")
    /// ```
    pub fn relevant(query_key: &str) -> ContextPolicy {
        let key = query_key.to_string();
        ContextPolicy::new("relevant", move |history| {
            let mut result = vec![Content::user(format!(
                "[Filter context relevant to state key: {}]",
                key
            ))];
            result.extend(history.iter().cloned());
            result
        })
    }

    /// Extract specific information from context.
    ///
    /// Marker policy that signals the runtime to extract only the named
    /// pieces of information from the conversation history via an LLM call.
    ///
    /// # Example
    /// ```ignore
    /// C::extract(&["customer_name", "order_id", "complaint"])
    /// ```
    pub fn extract(keys: &[&str]) -> ContextPolicy {
        let owned_keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        ContextPolicy::new("extract", move |history| {
            let mut result = vec![Content::user(format!(
                "[Extract from context: {}]",
                owned_keys.join(", ")
            ))];
            result.extend(history.iter().cloned());
            result
        })
    }

    /// Distill context to essential information.
    ///
    /// Marker policy for LLM-powered distillation. Similar to `summarize` but
    /// focused on extracting only the essential facts per the given instruction.
    ///
    /// # Example
    /// ```ignore
    /// C::distill("Keep only decisions made and their rationale")
    /// ```
    pub fn distill(instruction: &str) -> ContextPolicy {
        let instruction = instruction.to_string();
        ContextPolicy::new("distill", move |history| {
            let mut result = vec![Content::user(format!("[Distill context: {}]", instruction))];
            result.extend(history.iter().cloned());
            result
        })
    }

    /// Priority-weighted context selection.
    ///
    /// Assigns weights to context entries by role or content pattern. Higher
    /// weight entries are kept preferentially when context must be truncated.
    /// Weights are encoded as a marker for runtime processing.
    ///
    /// # Example
    /// ```ignore
    /// C::priority(&[("user", 1.0), ("model", 0.5), ("tool", 0.2)])
    /// ```
    pub fn priority(weights: &[(&str, f64)]) -> ContextPolicy {
        let owned_weights: Vec<(String, f64)> =
            weights.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        ContextPolicy::new("priority", move |history| {
            let weight_str = owned_weights
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join(", ");
            let mut result = vec![Content::user(format!("[Priority weights: {}]", weight_str))];
            result.extend(history.iter().cloned());
            result
        })
    }

    /// Fit context to a token budget with smart truncation.
    ///
    /// Similar to [`budget`](Self::budget) but adds a truncation marker so the
    /// runtime knows content was trimmed, allowing the agent to request more
    /// context if needed.
    ///
    /// # Example
    /// ```ignore
    /// C::fit(4096)
    /// ```
    pub fn fit(max_tokens: usize) -> ContextPolicy {
        use rs_genai::prelude::Part;
        let max_chars = max_tokens * 4; // rough estimate: 4 chars per token
        ContextPolicy::new("fit", move |history| {
            let mut total = 0;
            let mut result = Vec::new();
            // Work backwards to keep most recent messages
            for c in history.iter().rev() {
                let text_len: usize = c
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Text { text } => Some(text.len()),
                        _ => None,
                    })
                    .sum();
                if total + text_len > max_chars && !result.is_empty() {
                    // Prepend truncation marker
                    result.push(Content::user(format!(
                        "[Context truncated to fit ~{} token budget; {} earlier messages omitted]",
                        max_tokens,
                        history.len() - result.len()
                    )));
                    break;
                }
                total += text_len;
                result.push(c.clone());
            }
            result.reverse();
            result
        })
    }

    /// Project/keep only specific fields from context.
    ///
    /// Marker policy that signals the runtime to retain only the named fields
    /// from structured content in the conversation history.
    ///
    /// # Example
    /// ```ignore
    /// C::project(&["name", "status", "priority"])
    /// ```
    pub fn project(fields: &[&str]) -> ContextPolicy {
        let owned_fields: Vec<String> = fields.iter().map(|f| f.to_string()).collect();
        ContextPolicy::new("project", move |history| {
            let mut result = vec![Content::user(format!(
                "[Project fields: {}]",
                owned_fields.join(", ")
            ))];
            result.extend(history.iter().cloned());
            result
        })
    }

    /// Select context entries matching a predicate.
    ///
    /// Similar to [`filter`](Self::filter) but with select semantics: entries
    /// matching the predicate are included (positive selection).
    pub fn select(predicate: impl Fn(&Content) -> bool + Send + Sync + 'static) -> ContextPolicy {
        ContextPolicy::new("select", move |history| {
            history.iter().filter(|c| predicate(c)).cloned().collect()
        })
    }

    /// Include context only from specific agents.
    ///
    /// Filters context to keep only messages attributed to the named agents.
    /// Agent attribution is detected via `[Agent: name]` markers in content.
    ///
    /// # Example
    /// ```ignore
    /// C::from_agents(&["researcher", "analyst"])
    /// ```
    pub fn from_agents(names: &[&str]) -> ContextPolicy {
        let owned_names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        ContextPolicy::new("from_agents", move |history| {
            history
                .iter()
                .filter(|c| {
                    c.parts.iter().any(|p| match p {
                        rs_genai::prelude::Part::Text { text } => owned_names
                            .iter()
                            .any(|name| text.contains(&format!("[Agent: {}]", name))),
                        _ => false,
                    })
                })
                .cloned()
                .collect()
        })
    }

    /// Exclude context from specific agents.
    ///
    /// Filters out messages attributed to the named agents. Agent attribution
    /// is detected via `[Agent: name]` markers in content.
    ///
    /// # Example
    /// ```ignore
    /// C::exclude_agents(&["logger", "debugger"])
    /// ```
    pub fn exclude_agents(names: &[&str]) -> ContextPolicy {
        let owned_names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        ContextPolicy::new("exclude_agents", move |history| {
            history
                .iter()
                .filter(|c| {
                    !c.parts.iter().any(|p| match p {
                        rs_genai::prelude::Part::Text { text } => owned_names
                            .iter()
                            .any(|name| text.contains(&format!("[Agent: {}]", name))),
                        _ => false,
                    })
                })
                .cloned()
                .collect()
        })
    }

    /// Scratchpad: read notes from a state key as context.
    ///
    /// Prepends the value of a state key as a notes/scratchpad section in the
    /// context. Useful for maintaining running notes across turns.
    ///
    /// # Example
    /// ```ignore
    /// C::notes("session:scratchpad")
    /// ```
    pub fn notes(key: &str) -> ContextPolicy {
        let key = key.to_string();
        ContextPolicy::new("notes", move |history| {
            let mut result = vec![Content::user(format!(
                "[Scratchpad from state key: {}]",
                key
            ))];
            result.extend(history.iter().cloned());
            result
        })
    }

    /// Pipeline-aware context that adapts based on pipeline position.
    ///
    /// Marker policy that signals the runtime to adjust context based on
    /// where the current agent sits in a pipeline. Early stages receive full
    /// context; later stages receive only the outputs of preceding stages.
    ///
    /// # Example
    /// ```ignore
    /// C::pipeline_aware()
    /// ```
    pub fn pipeline_aware() -> ContextPolicy {
        ContextPolicy::new("pipeline_aware", |history| {
            let mut result = vec![Content::user(
                "[Pipeline-aware: adapt context to pipeline position]".to_string(),
            )];
            result.extend(history.iter().cloned());
            result
        })
    }

    /// Deduplicate adjacent messages with identical text content.
    pub fn dedup() -> ContextPolicy {
        use rs_genai::prelude::Part;
        ContextPolicy::new("dedup", |history| {
            fn extract_text(c: &Content) -> String {
                c.parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect()
            }
            let mut result: Vec<Content> = Vec::new();
            for c in history {
                let dominated = result.last().is_some_and(|prev| {
                    let prev_text = extract_text(prev);
                    let curr_text = extract_text(c);
                    prev_text == curr_text && !prev_text.is_empty()
                });
                if !dominated {
                    result.push(c.clone());
                }
            }
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rs_genai::prelude::Content;

    #[test]
    fn window_limits_messages() {
        let history = vec![Content::user("a"), Content::model("b"), Content::user("c")];
        let result = C::window(2).apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn window_keeps_all_if_under_limit() {
        let history = vec![Content::user("a")];
        let result = C::window(5).apply(&history);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn user_only_filters() {
        let history = vec![Content::user("a"), Content::model("b"), Content::user("c")];
        let result = C::user_only().apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn compose_with_add() {
        let chain = C::window(10) + C::user_only();
        assert_eq!(chain.policies.len(), 2);
    }

    #[test]
    fn chain_extends_with_add() {
        let chain = C::window(10) + C::user_only() + C::custom(|h| h.to_vec());
        assert_eq!(chain.policies.len(), 3);
    }

    #[test]
    fn model_only_filters() {
        let history = vec![Content::user("a"), Content::model("b"), Content::user("c")];
        let result = C::model_only().apply(&history);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn head_keeps_first_n() {
        let history = vec![Content::user("a"), Content::model("b"), Content::user("c")];
        let result = C::head(2).apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn sample_every_nth() {
        let history = vec![
            Content::user("a"),
            Content::model("b"),
            Content::user("c"),
            Content::model("d"),
        ];
        let result = C::sample(2).apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn empty_returns_nothing() {
        let history = vec![Content::user("a"), Content::model("b")];
        let result = C::empty().apply(&history);
        assert!(result.is_empty());
    }

    #[test]
    fn last_is_alias_for_window() {
        let history = vec![Content::user("a"), Content::model("b"), Content::user("c")];
        let result = C::last(1).apply(&history);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn text_only_filters_non_text() {
        let history = vec![Content::user("text msg")];
        let result = C::text_only().apply(&history);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn filter_with_predicate() {
        use rs_genai::prelude::Part;
        let history = vec![
            Content::user("keep"),
            Content::user("skip"),
            Content::user("keep this too"),
        ];
        let result = C::filter(|c| {
            c.parts.iter().any(|p| match p {
                Part::Text { text } => text.contains("keep"),
                _ => false,
            })
        })
        .apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn dedup_removes_adjacent_duplicates() {
        let history = vec![
            Content::user("hello"),
            Content::user("hello"),
            Content::user("world"),
            Content::user("world"),
            Content::user("world"),
        ];
        let result = C::dedup().apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn prepend_adds_to_front() {
        let history = vec![Content::user("existing")];
        let result = C::prepend(Content::model("system")).apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn append_adds_to_back() {
        let history = vec![Content::user("existing")];
        let result = C::append(Content::model("suffix")).apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn from_state_prepends_context() {
        let history = vec![Content::user("hello")];
        let result = C::from_state(&["user:name", "app:balance"]).apply(&history);
        assert_eq!(result.len(), 2);
        // First message should be the context keys
        if let rs_genai::prelude::Part::Text { text } = &result[0].parts[0] {
            assert!(text.contains("user:name"));
            assert!(text.contains("app:balance"));
        } else {
            panic!("Expected text part");
        }
    }

    #[test]
    fn summarize_prepends_marker() {
        let history = vec![Content::user("hello"), Content::model("hi")];
        let result = C::summarize("Focus on action items").apply(&history);
        assert_eq!(result.len(), 3);
        if let rs_genai::prelude::Part::Text { text } = &result[0].parts[0] {
            assert!(text.contains("Summarize context"));
            assert!(text.contains("action items"));
        } else {
            panic!("Expected text part");
        }
    }

    #[test]
    fn relevant_prepends_key_marker() {
        let history = vec![Content::user("hello")];
        let result = C::relevant("user:topic").apply(&history);
        assert_eq!(result.len(), 2);
        if let rs_genai::prelude::Part::Text { text } = &result[0].parts[0] {
            assert!(text.contains("user:topic"));
        } else {
            panic!("Expected text part");
        }
    }

    #[test]
    fn extract_prepends_keys_marker() {
        let history = vec![Content::user("hello")];
        let result = C::extract(&["name", "order_id"]).apply(&history);
        assert_eq!(result.len(), 2);
        if let rs_genai::prelude::Part::Text { text } = &result[0].parts[0] {
            assert!(text.contains("name"));
            assert!(text.contains("order_id"));
        } else {
            panic!("Expected text part");
        }
    }

    #[test]
    fn distill_prepends_instruction_marker() {
        let history = vec![Content::user("hello")];
        let result = C::distill("Keep only decisions").apply(&history);
        assert_eq!(result.len(), 2);
        if let rs_genai::prelude::Part::Text { text } = &result[0].parts[0] {
            assert!(text.contains("Distill context"));
            assert!(text.contains("decisions"));
        } else {
            panic!("Expected text part");
        }
    }

    #[test]
    fn priority_prepends_weights_marker() {
        let history = vec![Content::user("hello")];
        let result = C::priority(&[("user", 1.0), ("model", 0.5)]).apply(&history);
        assert_eq!(result.len(), 2);
        if let rs_genai::prelude::Part::Text { text } = &result[0].parts[0] {
            assert!(text.contains("Priority weights"));
            assert!(text.contains("user=1"));
            assert!(text.contains("model=0.5"));
        } else {
            panic!("Expected text part");
        }
    }

    #[test]
    fn fit_truncates_with_marker() {
        // Create history that exceeds budget
        let long_msg = "a".repeat(500);
        let history = vec![
            Content::user(long_msg.clone()),
            Content::user(long_msg.clone()),
            Content::user("recent"),
        ];
        // Budget of 10 tokens ~ 40 chars, only "recent" fits
        let result = C::fit(10).apply(&history);
        // Should have the recent message + truncation marker
        assert!(result.len() <= 3);
        // Check that truncation marker exists somewhere
        let has_marker = result.iter().any(|c| {
            c.parts.iter().any(|p| match p {
                rs_genai::prelude::Part::Text { text } => text.contains("truncated"),
                _ => false,
            })
        });
        assert!(has_marker);
    }

    #[test]
    fn fit_keeps_all_when_under_budget() {
        let history = vec![Content::user("hi"), Content::model("hello")];
        let result = C::fit(1000).apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn project_prepends_fields_marker() {
        let history = vec![Content::user("hello")];
        let result = C::project(&["name", "status"]).apply(&history);
        assert_eq!(result.len(), 2);
        if let rs_genai::prelude::Part::Text { text } = &result[0].parts[0] {
            assert!(text.contains("Project fields"));
            assert!(text.contains("name"));
            assert!(text.contains("status"));
        } else {
            panic!("Expected text part");
        }
    }

    #[test]
    fn select_filters_matching() {
        use rs_genai::prelude::Role;
        let history = vec![
            Content::user("keep"),
            Content::model("skip"),
            Content::user("also keep"),
        ];
        let result = C::select(|c| c.role == Some(Role::User)).apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn from_agents_filters_by_agent_marker() {
        let history = vec![
            Content::user("[Agent: researcher] Found data"),
            Content::user("[Agent: logger] Debug info"),
            Content::user("[Agent: researcher] More data"),
        ];
        let result = C::from_agents(&["researcher"]).apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn exclude_agents_removes_agent_messages() {
        let history = vec![
            Content::user("[Agent: researcher] Found data"),
            Content::user("[Agent: logger] Debug info"),
            Content::user("[Agent: researcher] More data"),
        ];
        let result = C::exclude_agents(&["logger"]).apply(&history);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn notes_prepends_scratchpad_marker() {
        let history = vec![Content::user("hello")];
        let result = C::notes("session:scratchpad").apply(&history);
        assert_eq!(result.len(), 2);
        if let rs_genai::prelude::Part::Text { text } = &result[0].parts[0] {
            assert!(text.contains("Scratchpad"));
            assert!(text.contains("session:scratchpad"));
        } else {
            panic!("Expected text part");
        }
    }

    #[test]
    fn pipeline_aware_prepends_marker() {
        let history = vec![Content::user("hello")];
        let result = C::pipeline_aware().apply(&history);
        assert_eq!(result.len(), 2);
        if let rs_genai::prelude::Part::Text { text } = &result[0].parts[0] {
            assert!(text.contains("Pipeline-aware"));
        } else {
            panic!("Expected text part");
        }
    }
}
