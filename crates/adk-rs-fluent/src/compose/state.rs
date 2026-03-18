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
    fn new(name: &'static str, f: impl Fn(&mut serde_json::Value) + Send + Sync + 'static) -> Self {
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
    /// The ordered list of transforms applied sequentially.
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

    /// Flatten a nested object key into the top level.
    pub fn flatten(key: &str) -> StateTransform {
        let key = key.to_string();
        StateTransform::new("flatten", move |state| {
            if let Some(obj) = state.as_object_mut() {
                if let Some(serde_json::Value::Object(nested)) = obj.remove(&key) {
                    for (k, v) in nested {
                        obj.insert(k, v);
                    }
                }
            }
        })
    }

    /// Set a key to a fixed value.
    pub fn set(key: &str, value: serde_json::Value) -> StateTransform {
        let key = key.to_string();
        StateTransform::new("set", move |state| {
            if let Some(obj) = state.as_object_mut() {
                obj.insert(key.clone(), value.clone());
            }
        })
    }

    // ── State predicates ───────────────────────────────────────────────────
    // Ergonomic helpers for transition guards and `.when()` predicates.

    /// Returns `true` if the given key exists with any non-null value.
    ///
    /// Replaces the common pattern `|s| s.get::<String>("key").is_some()`.
    ///
    /// ```ignore
    /// .transition("next_phase", S::is_set("caller_name"))
    /// ```
    pub fn is_set(key: &str) -> impl Fn(&rs_adk::State) -> bool + Send + Sync + 'static {
        let key = key.to_string();
        move |s: &rs_adk::State| s.contains(&key)
    }

    /// Returns `true` if the given key holds a truthy boolean.
    ///
    /// ```ignore
    /// .transition("next_phase", S::is_true("disclosure_given"))
    /// ```
    pub fn is_true(key: &str) -> impl Fn(&rs_adk::State) -> bool + Send + Sync + 'static {
        let key = key.to_string();
        move |s: &rs_adk::State| s.get::<bool>(&key).unwrap_or(false)
    }

    /// Returns `true` if the given key equals the expected string value.
    ///
    /// ```ignore
    /// .transition("tech:greet", S::eq("issue_type", "technical"))
    /// ```
    pub fn eq(
        key: &str,
        expected: &str,
    ) -> impl Fn(&rs_adk::State) -> bool + Send + Sync + 'static {
        let key = key.to_string();
        let expected = expected.to_string();
        move |s: &rs_adk::State| {
            s.get::<String>(&key)
                .map(|v| v == expected)
                .unwrap_or(false)
        }
    }

    /// Returns `true` if the given key matches any of the provided string values.
    ///
    /// ```ignore
    /// .transition("arrange_payment", S::one_of("negotiation_intent", &["full_pay", "partial_pay"]))
    /// ```
    pub fn one_of(
        key: &str,
        values: &[&str],
    ) -> impl Fn(&rs_adk::State) -> bool + Send + Sync + 'static {
        let key = key.to_string();
        let values: Vec<String> = values.iter().map(|v| v.to_string()).collect();
        move |s: &rs_adk::State| s.get::<String>(&key).is_some_and(|v| values.contains(&v))
    }

    /// Transform a single key's value with a function.
    pub fn transform(
        key: &str,
        f: impl Fn(serde_json::Value) -> serde_json::Value + Send + Sync + 'static,
    ) -> StateTransform {
        let key = key.to_string();
        StateTransform::new("transform", move |state| {
            if let Some(obj) = state.as_object_mut() {
                if let Some(val) = obj.remove(&key) {
                    obj.insert(key.clone(), f(val));
                }
            }
        })
    }

    /// Guard — assert a condition on state, panic with message if false.
    pub fn guard(
        predicate: impl Fn(&serde_json::Value) -> bool + Send + Sync + 'static,
        msg: &str,
    ) -> StateTransform {
        let msg = msg.to_string();
        StateTransform::new("guard", move |state| {
            assert!(predicate(state), "{}", msg);
        })
    }

    /// Compute derived values from state.
    pub fn compute(
        key: &str,
        f: impl Fn(&serde_json::Value) -> serde_json::Value + Send + Sync + 'static,
    ) -> StateTransform {
        let key = key.to_string();
        StateTransform::new("compute", move |state| {
            let val = f(state);
            if let Some(obj) = state.as_object_mut() {
                obj.insert(key.clone(), val);
            }
        })
    }

    /// Accumulate values into a list under a target key.
    pub fn accumulate(source_key: &str, into: &str) -> StateTransform {
        let source = source_key.to_string();
        let into = into.to_string();
        StateTransform::new("accumulate", move |state| {
            if let Some(obj) = state.as_object_mut() {
                if let Some(val) = obj.get(&source).cloned() {
                    let arr = obj
                        .entry(into.clone())
                        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                    if let Some(arr) = arr.as_array_mut() {
                        arr.push(val);
                    }
                }
            }
        })
    }

    /// Increment a counter key by a step.
    pub fn counter(key: &str, step: i64) -> StateTransform {
        let key = key.to_string();
        StateTransform::new("counter", move |state| {
            if let Some(obj) = state.as_object_mut() {
                let current = obj.get(&key).and_then(|v| v.as_i64()).unwrap_or(0);
                obj.insert(key.clone(), serde_json::json!(current + step));
            }
        })
    }

    /// Require that specified keys exist.
    pub fn require(keys: &[&str]) -> StateTransform {
        let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        StateTransform::new("require", move |state| {
            if let Some(obj) = state.as_object() {
                for key in &keys {
                    assert!(
                        obj.contains_key(key),
                        "Required key '{}' missing from state",
                        key
                    );
                }
            }
        })
    }

    /// Identity transform — no-op passthrough.
    pub fn identity() -> StateTransform {
        StateTransform::new("identity", |_| {})
    }

    /// Conditional transform — applies inner transform only when predicate is true.
    pub fn when(
        predicate: impl Fn(&serde_json::Value) -> bool + Send + Sync + 'static,
        inner: StateTransform,
    ) -> StateTransform {
        StateTransform::new("when", move |state| {
            if predicate(state) {
                inner.apply(state);
            }
        })
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

    /// Log a message during state transform (side-effect marker).
    ///
    /// Useful for debugging transform pipelines — prints the message to stderr
    /// each time the transform is applied.
    ///
    /// ```ignore
    /// let chain = S::pick(&["a"]) >> S::log("after pick") >> S::rename(&[("a", "x")]);
    /// ```
    pub fn log(message: &str) -> StateTransform {
        let message = message.to_string();
        StateTransform::new("log", move |_state| {
            eprintln!("[S::log] {}", message);
        })
    }

    /// Unflatten a dotted-key object into a nested structure (inverse of `flatten`).
    ///
    /// Takes all top-level keys that start with `key.` and nests them under `key` as an object.
    /// For example, `{"addr.city": "NYC", "addr.zip": "10001"}` with `unflatten("addr")`
    /// becomes `{"addr": {"city": "NYC", "zip": "10001"}}`.
    ///
    /// ```ignore
    /// let t = S::unflatten("addr");
    /// ```
    pub fn unflatten(key: &str) -> StateTransform {
        let key = key.to_string();
        StateTransform::new("unflatten", move |state| {
            if let Some(obj) = state.as_object_mut() {
                let prefix = format!("{}.", key);
                let dotted: Vec<(String, serde_json::Value)> = obj
                    .keys()
                    .filter(|k| k.starts_with(&prefix))
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .filter_map(|k| obj.remove(&k).map(|v| (k, v)))
                    .collect();

                if !dotted.is_empty() {
                    let nested = obj
                        .entry(key.clone())
                        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                    if let Some(nested_obj) = nested.as_object_mut() {
                        for (k, v) in dotted {
                            let sub_key = k[prefix.len()..].to_string();
                            nested_obj.insert(sub_key, v);
                        }
                    }
                }
            }
        })
    }

    /// Zip multiple array keys into an array of tuples (arrays).
    ///
    /// Takes the arrays at each of `keys` and produces an array of arrays under `into`,
    /// where element `i` contains `[keys[0][i], keys[1][i], ...]`.
    /// Arrays are zipped to the length of the shortest.
    ///
    /// ```ignore
    /// // {"names": ["a","b"], "scores": [1,2]} -> {"zipped": [["a",1], ["b",2]]}
    /// let t = S::zip(&["names", "scores"], "zipped");
    /// ```
    pub fn zip(keys: &[&str], into: &str) -> StateTransform {
        let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        let into = into.to_string();
        StateTransform::new("zip", move |state| {
            if let Some(obj) = state.as_object_mut() {
                let arrays: Vec<&Vec<serde_json::Value>> = keys
                    .iter()
                    .filter_map(|k| obj.get(k).and_then(|v| v.as_array()))
                    .collect();

                if arrays.len() == keys.len() {
                    let min_len = arrays.iter().map(|a| a.len()).min().unwrap_or(0);
                    let mut zipped = Vec::with_capacity(min_len);
                    for i in 0..min_len {
                        let tuple: Vec<serde_json::Value> =
                            arrays.iter().map(|a| a[i].clone()).collect();
                        zipped.push(serde_json::Value::Array(tuple));
                    }
                    obj.insert(into.clone(), serde_json::Value::Array(zipped));
                }
            }
        })
    }

    /// Group array elements by a field value.
    ///
    /// Takes the array at `source`, groups its elements by the string value of `key`,
    /// and writes the resulting object (field value -> array of elements) to `into`.
    ///
    /// ```ignore
    /// // {"items": [{"type":"a","v":1}, {"type":"b","v":2}, {"type":"a","v":3}]}
    /// // -> {"grouped": {"a": [{"type":"a","v":1}, {"type":"a","v":3}], "b": [{"type":"b","v":2}]}}
    /// let t = S::group_by("items", "type", "grouped");
    /// ```
    pub fn group_by(source: &str, key: &str, into: &str) -> StateTransform {
        let source = source.to_string();
        let key = key.to_string();
        let into = into.to_string();
        StateTransform::new("group_by", move |state| {
            if let Some(obj) = state.as_object_mut() {
                if let Some(arr) = obj.get(&source).and_then(|v| v.as_array()) {
                    let mut groups: serde_json::Map<String, serde_json::Value> =
                        serde_json::Map::new();
                    for item in arr {
                        let group_key = item
                            .get(&key)
                            .and_then(|v| v.as_str())
                            .unwrap_or("_unknown")
                            .to_string();
                        let group = groups
                            .entry(group_key)
                            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                        if let Some(arr) = group.as_array_mut() {
                            arr.push(item.clone());
                        }
                    }
                    obj.insert(into.clone(), serde_json::Value::Object(groups));
                }
            }
        })
    }

    /// Keep history of a key's values (append to list, cap at max).
    ///
    /// Each time this transform runs, the current value of `key` is appended to
    /// `{key}_history`. The history array is capped at `max` entries (oldest dropped).
    ///
    /// ```ignore
    /// let t = S::history("score", 5); // keeps last 5 score values in "score_history"
    /// ```
    pub fn history(key: &str, max: usize) -> StateTransform {
        let key = key.to_string();
        StateTransform::new("history", move |state| {
            if let Some(obj) = state.as_object_mut() {
                let history_key = format!("{}_history", key);
                if let Some(val) = obj.get(&key).cloned() {
                    let arr = obj
                        .entry(history_key)
                        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                    if let Some(arr) = arr.as_array_mut() {
                        arr.push(val);
                        while arr.len() > max {
                            arr.remove(0);
                        }
                    }
                }
            }
        })
    }

    /// Validate state against a JSON schema value.
    ///
    /// Panics with a descriptive message if any required key from the schema's
    /// `required` array is missing, or if a key's type doesn't match the schema's
    /// `properties.{key}.type` declaration.
    ///
    /// ```ignore
    /// let t = S::validate(json!({
    ///     "required": ["name", "age"],
    ///     "properties": {
    ///         "name": {"type": "string"},
    ///         "age": {"type": "number"}
    ///     }
    /// }));
    /// ```
    pub fn validate(schema: serde_json::Value) -> StateTransform {
        StateTransform::new("validate", move |state| {
            if let Some(obj) = state.as_object() {
                // Check required keys
                if let Some(required) = schema.get("required").and_then(|v| v.as_array()) {
                    for req in required {
                        if let Some(key) = req.as_str() {
                            assert!(
                                obj.contains_key(key),
                                "Validation failed: required key '{}' missing from state",
                                key
                            );
                        }
                    }
                }
                // Check property types
                if let Some(properties) = schema.get("properties").and_then(|v| v.as_object()) {
                    for (key, prop_schema) in properties {
                        if let Some(val) = obj.get(key) {
                            if let Some(expected_type) =
                                prop_schema.get("type").and_then(|v| v.as_str())
                            {
                                let actual_ok = match expected_type {
                                    "string" => val.is_string(),
                                    "number" | "integer" => val.is_number(),
                                    "boolean" => val.is_boolean(),
                                    "array" => val.is_array(),
                                    "object" => val.is_object(),
                                    "null" => val.is_null(),
                                    _ => true,
                                };
                                assert!(
                                    actual_ok,
                                    "Validation failed: key '{}' expected type '{}', got {:?}",
                                    key, expected_type, val
                                );
                            }
                        }
                    }
                }
            }
        })
    }

    /// Conditional branching of state transforms.
    ///
    /// Applies `if_true` when the predicate returns `true`, otherwise applies `if_false`.
    ///
    /// ```ignore
    /// let t = S::branch(
    ///     |s| s.get("premium").and_then(|v| v.as_bool()).unwrap_or(false),
    ///     S::set("tier", json!("gold")),
    ///     S::set("tier", json!("basic")),
    /// );
    /// ```
    pub fn branch(
        predicate: impl Fn(&serde_json::Value) -> bool + Send + Sync + 'static,
        if_true: StateTransform,
        if_false: StateTransform,
    ) -> StateTransform {
        StateTransform::new("branch", move |state| {
            if predicate(state) {
                if_true.apply(state);
            } else {
                if_false.apply(state);
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
    fn flatten_nested_object() {
        let mut state = json!({"nested": {"x": 1, "y": 2}, "z": 3});
        S::flatten("nested").apply(&mut state);
        assert_eq!(state, json!({"x": 1, "y": 2, "z": 3}));
    }

    #[test]
    fn flatten_missing_key_is_noop() {
        let mut state = json!({"a": 1});
        S::flatten("nonexistent").apply(&mut state);
        assert_eq!(state, json!({"a": 1}));
    }

    #[test]
    fn set_inserts_value() {
        let mut state = json!({"a": 1});
        S::set("b", json!(42)).apply(&mut state);
        assert_eq!(state, json!({"a": 1, "b": 42}));
    }

    #[test]
    fn set_overwrites_existing() {
        let mut state = json!({"a": 1});
        S::set("a", json!("replaced")).apply(&mut state);
        assert_eq!(state, json!({"a": "replaced"}));
    }

    #[test]
    fn chain_extends() {
        let chain = S::pick(&["a"]) >> S::rename(&[("a", "b")]) >> S::defaults(json!({"c": 99}));
        let mut state = json!({"a": 1, "x": 2});
        chain.apply(&mut state);
        assert_eq!(state, json!({"b": 1, "c": 99}));
    }

    #[test]
    fn log_is_noop_on_state() {
        let mut state = json!({"a": 1});
        S::log("debug message").apply(&mut state);
        assert_eq!(state, json!({"a": 1}));
    }

    #[test]
    fn unflatten_groups_dotted_keys() {
        let mut state = json!({"addr.city": "NYC", "addr.zip": "10001", "name": "Alice"});
        S::unflatten("addr").apply(&mut state);
        assert_eq!(
            state,
            json!({"name": "Alice", "addr": {"city": "NYC", "zip": "10001"}})
        );
    }

    #[test]
    fn unflatten_missing_prefix_is_noop() {
        let mut state = json!({"a": 1});
        S::unflatten("addr").apply(&mut state);
        assert_eq!(state, json!({"a": 1}));
    }

    #[test]
    fn zip_combines_arrays() {
        let mut state = json!({"names": ["a", "b", "c"], "scores": [10, 20, 30]});
        S::zip(&["names", "scores"], "zipped").apply(&mut state);
        assert_eq!(
            state["zipped"],
            json!([["a", 10], ["b", 20], ["c", 30]])
        );
    }

    #[test]
    fn zip_truncates_to_shortest() {
        let mut state = json!({"a": [1, 2, 3], "b": [10, 20]});
        S::zip(&["a", "b"], "z").apply(&mut state);
        assert_eq!(state["z"], json!([[1, 10], [2, 20]]));
    }

    #[test]
    fn group_by_groups_elements() {
        let mut state = json!({
            "items": [
                {"type": "fruit", "name": "apple"},
                {"type": "veg", "name": "carrot"},
                {"type": "fruit", "name": "banana"}
            ]
        });
        S::group_by("items", "type", "grouped").apply(&mut state);
        let grouped = &state["grouped"];
        assert_eq!(grouped["fruit"].as_array().unwrap().len(), 2);
        assert_eq!(grouped["veg"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn history_tracks_values() {
        let mut state = json!({"score": 10});
        let t = S::history("score", 3);
        t.apply(&mut state);
        state["score"] = json!(20);
        t.apply(&mut state);
        state["score"] = json!(30);
        t.apply(&mut state);
        state["score"] = json!(40);
        t.apply(&mut state);
        // Should only keep last 3
        assert_eq!(state["score_history"], json!([20, 30, 40]));
    }

    #[test]
    fn validate_passes_valid_state() {
        let mut state = json!({"name": "Alice", "age": 30});
        S::validate(json!({
            "required": ["name", "age"],
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "number"}
            }
        }))
        .apply(&mut state);
        // Should not panic
    }

    #[test]
    #[should_panic(expected = "required key 'missing' missing from state")]
    fn validate_fails_missing_required() {
        let mut state = json!({"name": "Alice"});
        S::validate(json!({"required": ["name", "missing"]})).apply(&mut state);
    }

    #[test]
    #[should_panic(expected = "expected type 'string'")]
    fn validate_fails_wrong_type() {
        let mut state = json!({"name": 42});
        S::validate(json!({
            "properties": {"name": {"type": "string"}}
        }))
        .apply(&mut state);
    }

    #[test]
    fn branch_takes_true_path() {
        let mut state = json!({"premium": true});
        S::branch(
            |s| s.get("premium").and_then(|v| v.as_bool()).unwrap_or(false),
            S::set("tier", json!("gold")),
            S::set("tier", json!("basic")),
        )
        .apply(&mut state);
        assert_eq!(state["tier"], "gold");
    }

    #[test]
    fn branch_takes_false_path() {
        let mut state = json!({"premium": false});
        S::branch(
            |s| s.get("premium").and_then(|v| v.as_bool()).unwrap_or(false),
            S::set("tier", json!("gold")),
            S::set("tier", json!("basic")),
        )
        .apply(&mut state);
        assert_eq!(state["tier"], "basic");
    }
}
