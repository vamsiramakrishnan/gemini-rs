//! S — State transforms.
//!
//! Compose state transformations sequentially with `>>`.

use std::sync::Arc;

/// A state transformation step.
#[derive(Clone)]
pub struct StateTransform {
    name: &'static str,
    transform: Arc<dyn Fn(&mut serde_json::Value) + Send + Sync>,
}

impl StateTransform {
    fn new(
        name: &'static str,
        f: impl Fn(&mut serde_json::Value) + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            transform: Arc::new(f),
        }
    }

    /// Apply this transform to a state value.
    pub fn apply(&self, state: &mut serde_json::Value) {
        (self.transform)(state);
    }

    /// Name of this transform.
    pub fn name(&self) -> &str {
        self.name
    }
}

impl std::fmt::Debug for StateTransform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateTransform")
            .field("name", &self.name)
            .finish()
    }
}

/// Compose two state transforms sequentially with `>>`.
impl std::ops::Shr for StateTransform {
    type Output = StateTransformChain;

    fn shr(self, rhs: StateTransform) -> Self::Output {
        StateTransformChain {
            steps: vec![self, rhs],
        }
    }
}

/// A chain of state transforms applied sequentially.
#[derive(Clone)]
pub struct StateTransformChain {
    pub steps: Vec<StateTransform>,
}

impl StateTransformChain {
    /// Apply all transforms in order.
    pub fn apply(&self, state: &mut serde_json::Value) {
        for step in &self.steps {
            step.apply(state);
        }
    }
}

/// Extend the chain with `>>`.
impl std::ops::Shr<StateTransform> for StateTransformChain {
    type Output = StateTransformChain;

    fn shr(mut self, rhs: StateTransform) -> Self::Output {
        self.steps.push(rhs);
        self
    }
}

/// The `S` namespace — static factory methods for state transforms.
pub struct S;

impl S {
    /// Keep only the specified keys.
    pub fn pick(keys: &[&str]) -> StateTransform {
        let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        StateTransform::new("pick", move |state| {
            if let Some(obj) = state.as_object_mut() {
                obj.retain(|k, _| keys.contains(k));
            }
        })
    }

    /// Rename keys according to the mappings.
    pub fn rename(mappings: &[(&str, &str)]) -> StateTransform {
        let mappings: Vec<(String, String)> = mappings
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        StateTransform::new("rename", move |state| {
            if let Some(obj) = state.as_object_mut() {
                for (from, to) in &mappings {
                    if let Some(val) = obj.remove(from) {
                        obj.insert(to.clone(), val);
                    }
                }
            }
        })
    }

    /// Merge the specified keys into a single key as an object.
    pub fn merge(keys: &[&str], into: &str) -> StateTransform {
        let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        let into = into.to_string();
        StateTransform::new("merge", move |state| {
            if let Some(obj) = state.as_object_mut() {
                let mut merged = serde_json::Map::new();
                for key in &keys {
                    if let Some(val) = obj.remove(key) {
                        merged.insert(key.clone(), val);
                    }
                }
                obj.insert(into.clone(), serde_json::Value::Object(merged));
            }
        })
    }

    /// Set default values for missing keys.
    pub fn defaults(defaults: serde_json::Value) -> StateTransform {
        StateTransform::new("defaults", move |state| {
            if let (Some(obj), Some(defaults_obj)) = (state.as_object_mut(), defaults.as_object()) {
                for (k, v) in defaults_obj {
                    obj.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        })
    }

    /// Apply a custom transformation function.
    pub fn map(f: impl Fn(&mut serde_json::Value) + Send + Sync + 'static) -> StateTransform {
        StateTransform::new("map", f)
    }

    /// Drop the specified keys.
    pub fn drop(keys: &[&str]) -> StateTransform {
        let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        StateTransform::new("drop", move |state| {
            if let Some(obj) = state.as_object_mut() {
                for key in &keys {
                    obj.remove(key);
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pick_keeps_only_specified_keys() {
        let mut state = json!({"a": 1, "b": 2, "c": 3});
        S::pick(&["a", "c"]).apply(&mut state);
        assert_eq!(state, json!({"a": 1, "c": 3}));
    }

    #[test]
    fn rename_renames_keys() {
        let mut state = json!({"old_name": 42});
        S::rename(&[("old_name", "new_name")]).apply(&mut state);
        assert_eq!(state, json!({"new_name": 42}));
    }

    #[test]
    fn merge_combines_keys() {
        let mut state = json!({"x": 1, "y": 2, "z": 3});
        S::merge(&["x", "y"], "combined").apply(&mut state);
        assert_eq!(state, json!({"z": 3, "combined": {"x": 1, "y": 2}}));
    }

    #[test]
    fn defaults_sets_missing() {
        let mut state = json!({"existing": "yes"});
        S::defaults(json!({"existing": "no", "missing": "added"})).apply(&mut state);
        assert_eq!(state["existing"], "yes");
        assert_eq!(state["missing"], "added");
    }

    #[test]
    fn drop_removes_keys() {
        let mut state = json!({"keep": 1, "remove": 2});
        S::drop(&["remove"]).apply(&mut state);
        assert_eq!(state, json!({"keep": 1}));
    }

    #[test]
    fn map_custom_transform() {
        let mut state = json!({"count": 5});
        S::map(|s| {
            if let Some(n) = s.get("count").and_then(|v| v.as_i64()) {
                s["count"] = json!(n * 2);
            }
        })
        .apply(&mut state);
        assert_eq!(state["count"], 10);
    }

    #[test]
    fn chain_with_shr() {
        let chain = S::pick(&["a", "b"]) >> S::rename(&[("a", "x")]);
        let mut state = json!({"a": 1, "b": 2, "c": 3});
        chain.apply(&mut state);
        assert_eq!(state, json!({"x": 1, "b": 2}));
    }

    #[test]
    fn chain_extends() {
        let chain = S::pick(&["a"]) >> S::rename(&[("a", "b")]) >> S::defaults(json!({"c": 99}));
        let mut state = json!({"a": 1, "x": 2});
        chain.apply(&mut state);
        assert_eq!(state, json!({"b": 1, "c": 99}));
    }
}
