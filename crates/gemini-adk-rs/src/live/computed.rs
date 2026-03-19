//! Computed (derived) state variables with dependency-ordered evaluation.
//!
//! Computed variables are pure functions of other state keys. The [`ComputedRegistry`]
//! maintains a topologically sorted list of [`ComputedVar`]s so that dependencies are
//! always evaluated before the variables that depend on them.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::state::State;

/// A computed state variable: a pure function of other state keys.
///
/// The `compute` closure receives the full [`State`] and returns an optional
/// [`Value`]. When it returns `Some(value)`, the result is written to
/// `derived:{key}` in state. When it returns `None`, the key is skipped
/// (no write, no change detection).
pub struct ComputedVar {
    /// The state key this computed variable writes to (prefixed with `derived:`).
    pub key: String,
    /// State keys this variable depends on.
    pub dependencies: Vec<String>,
    /// Closure that computes the derived value from current state.
    pub compute: Arc<dyn Fn(&State) -> Option<Value> + Send + Sync>,
}

/// Registry of computed variables with dependency-ordered evaluation.
///
/// Variables are kept in topological order: if var A depends on var B, then B
/// appears before A in the internal list. This invariant is maintained at
/// registration time using Kahn's algorithm.
pub struct ComputedRegistry {
    /// Topologically sorted computed variables.
    vars: Vec<ComputedVar>,
    /// Maps a state key to the indices (into `vars`) of computed variables
    /// that list that key as a dependency.
    dep_index: HashMap<String, Vec<usize>>,
}

impl Default for ComputedRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ComputedRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            vars: Vec::new(),
            dep_index: HashMap::new(),
        }
    }

    /// Register a computed variable. Re-sorts the internal list and rebuilds
    /// the dependency index. **Panics** if the new variable introduces a cycle.
    pub fn register(&mut self, var: ComputedVar) {
        if let Some(pos) = self.vars.iter().position(|v| v.key == var.key) {
            self.vars[pos] = var; // replace existing
        } else {
            self.vars.push(var);
        }
        self.topo_sort_or_panic();
        self.rebuild_dep_index();
    }

    /// Recompute all variables in dependency order. Returns the keys whose
    /// derived values actually changed (old != new).
    pub fn recompute(&self, state: &State) -> Vec<String> {
        let mut changed = Vec::new();
        for var in &self.vars {
            if let Some(new_val) = (var.compute)(state) {
                let derived_key = format!("derived:{}", var.key);
                let old_val = state.get_raw(&derived_key);
                let did_change = old_val.as_ref() != Some(&new_val);
                state.set(&derived_key, new_val);
                if did_change {
                    changed.push(var.key.clone());
                }
            }
        }
        changed
    }

    /// Recompute only the variables affected by the given changed keys.
    /// Uses the dependency index for O(1) lookup of affected variables, then
    /// evaluates them in topological order. Transitively propagates: if a
    /// computed var changes, its dependents are also scheduled for recomputation.
    /// Returns keys that actually changed.
    pub fn recompute_affected(&self, state: &State, changed_keys: &[String]) -> Vec<String> {
        // Collect indices of affected vars transitively (deduplicated via bitmap).
        let mut visited = vec![false; self.vars.len()];
        let mut affected_set = Vec::new();

        // Seed the work queue with the initial changed keys.
        let mut work_keys: Vec<String> = changed_keys.to_vec();

        while let Some(key) = work_keys.pop() {
            // Look up vars that depend on this key directly.
            if let Some(indices) = self.dep_index.get(&key) {
                for &idx in indices {
                    if !visited[idx] {
                        visited[idx] = true;
                        affected_set.push(idx);
                        // This computed var's output (derived:<key>) might be
                        // a dependency of other vars, so enqueue it.
                        work_keys.push(self.vars[idx].key.clone());
                    }
                }
            }
        }

        // Sort by topological order (indices are already in topo order).
        affected_set.sort_unstable();

        let mut changed = Vec::new();
        for idx in affected_set {
            let var = &self.vars[idx];
            if let Some(new_val) = (var.compute)(state) {
                let derived_key = format!("derived:{}", var.key);
                let old_val = state.get_raw(&derived_key);
                let did_change = old_val.as_ref() != Some(&new_val);
                state.set(&derived_key, new_val);
                if did_change {
                    changed.push(var.key.clone());
                }
            }
        }
        changed
    }

    /// Validate the dependency graph. Returns `Ok(())` if there are no cycles,
    /// or `Err(message)` describing the problem.
    pub fn validate(&self) -> Result<(), String> {
        // Build adjacency from the current vars and run Kahn's algorithm.
        let n = self.vars.len();
        if n == 0 {
            return Ok(());
        }

        let key_to_idx: HashMap<&str, usize> = self
            .vars
            .iter()
            .enumerate()
            .map(|(i, v)| (v.key.as_str(), i))
            .collect();

        let mut in_degree = vec![0usize; n];
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

        for (i, var) in self.vars.iter().enumerate() {
            for dep in &var.dependencies {
                if let Some(&dep_idx) = key_to_idx.get(dep.as_str()) {
                    adj[dep_idx].push(i);
                    in_degree[i] += 1;
                }
                // External dependencies (not in registry) are fine — ignore them.
            }
        }

        let mut queue: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
        let mut visited = 0usize;

        while let Some(node) = queue.pop() {
            visited += 1;
            for &neighbor in &adj[node] {
                in_degree[neighbor] -= 1;
                if in_degree[neighbor] == 0 {
                    queue.push(neighbor);
                }
            }
        }

        if visited == n {
            Ok(())
        } else {
            // Find the vars involved in the cycle.
            let cycle_vars: Vec<&str> = (0..n)
                .filter(|&i| in_degree[i] > 0)
                .map(|i| self.vars[i].key.as_str())
                .collect();
            Err(format!(
                "Cycle detected among computed variables: {:?}",
                cycle_vars
            ))
        }
    }

    /// Returns the number of registered computed variables.
    pub fn len(&self) -> usize {
        self.vars.len()
    }

    /// Returns true if no computed variables are registered.
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    // ── Internal helpers ──────────────────────────────────────────────────

    /// Topologically sort `self.vars` in-place using Kahn's algorithm.
    /// Panics if a cycle is detected (including self-cycles).
    fn topo_sort_or_panic(&mut self) {
        let n = self.vars.len();

        // Check for self-cycles (a var depending on itself).
        for var in &self.vars {
            if var.dependencies.contains(&var.key) {
                panic!(
                    "Cycle detected among computed variables: {:?}",
                    vec![var.key.as_str()]
                );
            }
        }

        if n <= 1 {
            return;
        }

        // Map computed-var keys to their current index.
        let key_to_idx: HashMap<&str, usize> = self
            .vars
            .iter()
            .enumerate()
            .map(|(i, v)| (v.key.as_str(), i))
            .collect();

        // Build adjacency list and in-degree array.
        // Edge dep_idx -> i means "dep must come before i".
        let mut in_degree = vec![0usize; n];
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

        for (i, var) in self.vars.iter().enumerate() {
            for dep in &var.dependencies {
                if let Some(&dep_idx) = key_to_idx.get(dep.as_str()) {
                    adj[dep_idx].push(i);
                    in_degree[i] += 1;
                }
            }
        }

        // Kahn's algorithm.
        let mut queue: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
        let mut order: Vec<usize> = Vec::with_capacity(n);

        while let Some(node) = queue.pop() {
            order.push(node);
            for &neighbor in &adj[node] {
                in_degree[neighbor] -= 1;
                if in_degree[neighbor] == 0 {
                    queue.push(neighbor);
                }
            }
        }

        if order.len() != n {
            let cycle_vars: Vec<&str> = (0..n)
                .filter(|&i| in_degree[i] > 0)
                .map(|i| self.vars[i].key.as_str())
                .collect();
            panic!("Cycle detected among computed variables: {:?}", cycle_vars);
        }

        // Reorder vars according to topological sort.
        // Use Option wrapping for safe index-based extraction.
        let mut slots: Vec<Option<ComputedVar>> = self.vars.drain(..).map(Some).collect();
        for &idx in &order {
            if let Some(var) = slots[idx].take() {
                self.vars.push(var);
            }
        }
    }

    /// Rebuild the `dep_index` mapping from dependency keys to var indices.
    fn rebuild_dep_index(&mut self) {
        self.dep_index.clear();
        for (i, var) in self.vars.iter().enumerate() {
            for dep in &var.dependencies {
                self.dep_index.entry(dep.clone()).or_default().push(i);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── 1. Single var register + recompute ──────────────────────────────

    #[test]
    fn single_var_register_and_recompute() {
        let mut registry = ComputedRegistry::new();
        registry.register(ComputedVar {
            key: "doubled".into(),
            dependencies: vec!["app:count".into()],
            compute: Arc::new(|state| {
                let count: i64 = state.get("app:count")?;
                Some(json!(count * 2))
            }),
        });

        let state = State::new();
        state.set("app:count", 5);

        let changed = registry.recompute(&state);
        assert_eq!(changed, vec!["doubled"]);
        assert_eq!(state.get::<i64>("derived:doubled"), Some(10));
    }

    // ── 2. Dependency ordering (B depends on A) ────────────────────────

    #[test]
    fn dependency_ordering() {
        let mut registry = ComputedRegistry::new();

        // Register B first (depends on derived:base).
        registry.register(ComputedVar {
            key: "derived_from_base".into(),
            dependencies: vec!["base".into()],
            compute: Arc::new(|state| {
                let base: i64 = state.get("derived:base")?;
                Some(json!(base + 100))
            }),
        });

        // Register A (base, no internal deps).
        registry.register(ComputedVar {
            key: "base".into(),
            dependencies: vec!["app:input".into()],
            compute: Arc::new(|state| {
                let input: i64 = state.get("app:input")?;
                Some(json!(input * 2))
            }),
        });

        let state = State::new();
        state.set("app:input", 3);

        let changed = registry.recompute(&state);
        // base should be computed first (6), then derived_from_base (106).
        assert_eq!(state.get::<i64>("derived:base"), Some(6));
        assert_eq!(state.get::<i64>("derived:derived_from_base"), Some(106));
        assert!(changed.contains(&"base".to_string()));
        assert!(changed.contains(&"derived_from_base".to_string()));
    }

    // ── 3. Cycle detection (panic) ─────────────────────────────────────

    #[test]
    #[should_panic(expected = "Cycle detected")]
    fn cycle_detection_panics() {
        let mut registry = ComputedRegistry::new();
        registry.register(ComputedVar {
            key: "a".into(),
            dependencies: vec!["b".into()],
            compute: Arc::new(|_| Some(json!(1))),
        });
        registry.register(ComputedVar {
            key: "b".into(),
            dependencies: vec!["a".into()],
            compute: Arc::new(|_| Some(json!(2))),
        });
    }

    // ── 4. Recompute returns only keys that changed ────────────────────

    #[test]
    fn recompute_returns_only_changed_keys() {
        let mut registry = ComputedRegistry::new();
        registry.register(ComputedVar {
            key: "level".into(),
            dependencies: vec!["app:score".into()],
            compute: Arc::new(|state| {
                let score: f64 = state.get("app:score")?;
                if score > 0.5 {
                    Some(json!("high"))
                } else {
                    Some(json!("low"))
                }
            }),
        });

        let state = State::new();
        state.set("app:score", 0.8);

        // First recompute: level is new, so it changed.
        let changed = registry.recompute(&state);
        assert_eq!(changed, vec!["level"]);

        // Second recompute with same input: no change.
        let changed = registry.recompute(&state);
        assert!(changed.is_empty());

        // Change input so derived value changes.
        state.set("app:score", 0.2);
        let changed = registry.recompute(&state);
        assert_eq!(changed, vec!["level"]);
        assert_eq!(
            state.get::<String>("derived:level"),
            Some("low".to_string())
        );
    }

    // ── 5. recompute_affected only recomputes affected vars ────────────

    #[test]
    fn recompute_affected_only_recomputes_affected() {
        let call_count_a = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let call_count_b = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let cc_a = call_count_a.clone();
        let cc_b = call_count_b.clone();

        let mut registry = ComputedRegistry::new();
        registry.register(ComputedVar {
            key: "from_x".into(),
            dependencies: vec!["app:x".into()],
            compute: Arc::new(move |state| {
                cc_a.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let x: i64 = state.get("app:x")?;
                Some(json!(x + 1))
            }),
        });
        registry.register(ComputedVar {
            key: "from_y".into(),
            dependencies: vec!["app:y".into()],
            compute: Arc::new(move |state| {
                cc_b.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let y: i64 = state.get("app:y")?;
                Some(json!(y + 1))
            }),
        });

        let state = State::new();
        state.set("app:x", 10);
        state.set("app:y", 20);

        // Only app:x changed — should only recompute from_x.
        let changed = registry.recompute_affected(&state, &["app:x".into()]);
        assert_eq!(changed, vec!["from_x"]);
        assert_eq!(call_count_a.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(call_count_b.load(std::sync::atomic::Ordering::SeqCst), 0);

        assert_eq!(state.get::<i64>("derived:from_x"), Some(11));
        // from_y was not computed, so derived:from_y should not exist.
        assert_eq!(state.get_raw("derived:from_y"), None);
    }

    // ── 6. validate catches cycles ─────────────────────────────────────

    #[test]
    fn validate_catches_cycles() {
        let mut registry = ComputedRegistry::new();
        // Manually push vars without going through register (which would panic).
        registry.vars.push(ComputedVar {
            key: "x".into(),
            dependencies: vec!["y".into()],
            compute: Arc::new(|_| Some(json!(1))),
        });
        registry.vars.push(ComputedVar {
            key: "y".into(),
            dependencies: vec!["x".into()],
            compute: Arc::new(|_| Some(json!(2))),
        });

        let result = registry.validate();
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("Cycle detected"));
    }

    // ── 7. validate succeeds on valid graph ────────────────────────────

    #[test]
    fn validate_succeeds_on_valid_graph() {
        let mut registry = ComputedRegistry::new();
        registry.register(ComputedVar {
            key: "a".into(),
            dependencies: vec!["app:input".into()],
            compute: Arc::new(|_| Some(json!(1))),
        });
        registry.register(ComputedVar {
            key: "b".into(),
            dependencies: vec!["a".into()],
            compute: Arc::new(|_| Some(json!(2))),
        });

        assert!(registry.validate().is_ok());
    }

    // ── 8. Compute returning None skips write ──────────────────────────

    #[test]
    fn compute_returning_none_skips_write() {
        let mut registry = ComputedRegistry::new();
        registry.register(ComputedVar {
            key: "maybe".into(),
            dependencies: vec!["app:flag".into()],
            compute: Arc::new(|state| {
                let flag: bool = state.get("app:flag")?;
                if flag {
                    Some(json!("yes"))
                } else {
                    None
                }
            }),
        });

        let state = State::new();
        // app:flag not set → get returns None → compute returns None.
        let changed = registry.recompute(&state);
        assert!(changed.is_empty());
        assert_eq!(state.get_raw("derived:maybe"), None);

        // Set flag to false → compute returns None.
        state.set("app:flag", false);
        let changed = registry.recompute(&state);
        assert!(changed.is_empty());
        assert_eq!(state.get_raw("derived:maybe"), None);

        // Set flag to true → compute returns Some.
        state.set("app:flag", true);
        let changed = registry.recompute(&state);
        assert_eq!(changed, vec!["maybe"]);
        assert_eq!(
            state.get::<String>("derived:maybe"),
            Some("yes".to_string())
        );
    }

    // ── 9. Diamond dependency ──────────────────────────────────────────

    #[test]
    fn diamond_dependency() {
        // D is the root. A and B depend on D. C depends on A and B.
        //
        //     D
        //    / \
        //   A   B
        //    \ /
        //     C
        let mut registry = ComputedRegistry::new();

        registry.register(ComputedVar {
            key: "d".into(),
            dependencies: vec!["app:root".into()],
            compute: Arc::new(|state| {
                let root: i64 = state.get("app:root")?;
                Some(json!(root))
            }),
        });

        registry.register(ComputedVar {
            key: "a".into(),
            dependencies: vec!["d".into()],
            compute: Arc::new(|state| {
                let d: i64 = state.get("derived:d")?;
                Some(json!(d + 10))
            }),
        });

        registry.register(ComputedVar {
            key: "b".into(),
            dependencies: vec!["d".into()],
            compute: Arc::new(|state| {
                let d: i64 = state.get("derived:d")?;
                Some(json!(d + 20))
            }),
        });

        registry.register(ComputedVar {
            key: "c".into(),
            dependencies: vec!["a".into(), "b".into()],
            compute: Arc::new(|state| {
                let a: i64 = state.get("derived:a")?;
                let b: i64 = state.get("derived:b")?;
                Some(json!(a + b))
            }),
        });

        let state = State::new();
        state.set("app:root", 1);

        let changed = registry.recompute(&state);
        assert_eq!(state.get::<i64>("derived:d"), Some(1));
        assert_eq!(state.get::<i64>("derived:a"), Some(11));
        assert_eq!(state.get::<i64>("derived:b"), Some(21));
        assert_eq!(state.get::<i64>("derived:c"), Some(32));
        assert_eq!(changed.len(), 4);
    }

    // ── 10. Empty registry recompute returns empty vec ─────────────────

    #[test]
    fn empty_registry_recompute_returns_empty() {
        let registry = ComputedRegistry::new();
        let state = State::new();
        let changed = registry.recompute(&state);
        assert!(changed.is_empty());
    }

    // ── Additional: len / is_empty ─────────────────────────────────────

    #[test]
    fn len_and_is_empty() {
        let mut registry = ComputedRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);

        registry.register(ComputedVar {
            key: "x".into(),
            dependencies: vec![],
            compute: Arc::new(|_| Some(json!(1))),
        });
        assert!(!registry.is_empty());
        assert_eq!(registry.len(), 1);
    }

    // ── Additional: recompute_affected with diamond ────────────────────

    #[test]
    fn recompute_affected_diamond() {
        let mut registry = ComputedRegistry::new();

        registry.register(ComputedVar {
            key: "root_derived".into(),
            dependencies: vec!["app:root".into()],
            compute: Arc::new(|state| {
                let r: i64 = state.get("app:root")?;
                Some(json!(r * 10))
            }),
        });

        registry.register(ComputedVar {
            key: "leaf".into(),
            dependencies: vec!["root_derived".into()],
            compute: Arc::new(|state| {
                let rd: i64 = state.get("derived:root_derived")?;
                Some(json!(rd + 5))
            }),
        });

        let state = State::new();
        state.set("app:root", 2);

        // First full recompute to populate.
        registry.recompute(&state);
        assert_eq!(state.get::<i64>("derived:root_derived"), Some(20));
        assert_eq!(state.get::<i64>("derived:leaf"), Some(25));

        // Now change root, use recompute_affected.
        state.set("app:root", 3);
        let changed = registry.recompute_affected(&state, &["app:root".into()]);
        // root_derived should be recomputed (depends on app:root).
        assert!(changed.contains(&"root_derived".to_string()));
        assert_eq!(state.get::<i64>("derived:root_derived"), Some(30));
        // leaf depends on root_derived — it should be picked up via
        // the dep_index entry for "root_derived".
        assert!(changed.contains(&"leaf".to_string()));
        assert_eq!(state.get::<i64>("derived:leaf"), Some(35));
    }

    // ── Additional: validate on empty registry ─────────────────────────

    #[test]
    fn validate_empty_registry() {
        let registry = ComputedRegistry::new();
        assert!(registry.validate().is_ok());
    }

    // ── Additional: self-cycle ──────────────────────────────────────────

    #[test]
    #[should_panic(expected = "Cycle detected")]
    fn self_cycle_panics() {
        let mut registry = ComputedRegistry::new();
        registry.register(ComputedVar {
            key: "self_ref".into(),
            dependencies: vec!["self_ref".into()],
            compute: Arc::new(|_| Some(json!(1))),
        });
    }
}
